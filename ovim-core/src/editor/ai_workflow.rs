use super::Editor;
use anyhow::{anyhow, Result};
use std::collections::BTreeMap;
use std::time::Instant;
use tokio::sync::mpsc::error::TryRecvError as MpscTryRecvError;
use tokio::sync::oneshot::error::TryRecvError as OneshotTryRecvError;

fn format_progress_message(event: &crate::ai::workflow::WorkflowProgressEvent) -> String {
    let phase = match event.kind {
        crate::ai::WorkflowStepProgressKind::Started => "running",
        crate::ai::WorkflowStepProgressKind::Completed => "completed",
    };
    match event.detail.as_deref() {
        Some(detail) if !detail.is_empty() => {
            format!("step '{}' {phase} ({detail})", event.step_id)
        }
        _ => format!("step '{}' {phase}", event.step_id),
    }
}

impl Editor {
    pub fn reload_workflows(&mut self) -> Result<usize> {
        let workflows = crate::ai::workflow::load_workflows()?;
        let count = workflows.len();
        self.ai_state.workflows = workflows;
        self.set_status_message(format!("Loaded {count} workflow(s)"));
        Ok(count)
    }

    pub fn workflow_names_sorted(&self) -> Vec<String> {
        let mut names: Vec<String> = self.ai_state.workflows.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn ensure_workflows_loaded(&mut self) -> Result<()> {
        if self.ai_state.workflows.is_empty() {
            self.reload_workflows()?;
        }
        Ok(())
    }

    pub fn run_workflow(
        &mut self,
        name: &str,
        inputs: BTreeMap<String, serde_json::Value>,
    ) -> Result<u64> {
        self.ensure_workflows_loaded()?;
        let spec = self
            .ai_state
            .workflows
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("unknown workflow '{}'", name))?;

        let run_id = self.ai_state.next_workflow_run_id;
        self.ai_state.next_workflow_run_id = self.ai_state.next_workflow_run_id.saturating_add(1);

        self.ai_state
            .workflow_runs
            .push(crate::ai::WorkflowRunRecord {
                id: run_id,
                workflow_name: name.to_string(),
                status: crate::ai::WorkflowRunStatus::Running,
                started_at: Instant::now(),
                finished_at: None,
                current_step: None,
                message: "running".to_string(),
                outputs: None,
            });

        let config = self.ai_state.config.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (progress_tx, progress_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let result = crate::ai::workflow::execute_workflow_with_progress(
                spec,
                inputs,
                config,
                Some(progress_tx),
            )
            .await;
            let cloned_for_channel = match &result {
                Ok(ok) => Ok(ok.clone()),
                Err(err) => Err(anyhow!(err.to_string())),
            };
            let _ = tx.send(cloned_for_channel);
        });

        self.ai_state
            .pending_workflow_runs
            .push(crate::ai::workflow::PendingWorkflowRun {
                run_id,
                receiver: rx,
                progress_receiver: progress_rx,
                task,
            });

        self.set_status_message(format!("Workflow '{}' started (run #{run_id})", name));
        Ok(run_id)
    }

    pub fn poll_pending_workflow_jobs(&mut self) -> bool {
        let mut changed = false;
        let mut idx = 0usize;

        while idx < self.ai_state.pending_workflow_runs.len() {
            let mut maybe_result = None;
            let mut progress_events = Vec::new();
            {
                let pending = &mut self.ai_state.pending_workflow_runs[idx];
                loop {
                    match pending.progress_receiver.try_recv() {
                        Ok(event) => progress_events.push((pending.run_id, event)),
                        Err(MpscTryRecvError::Empty) => break,
                        Err(MpscTryRecvError::Disconnected) => break,
                    }
                }
                match pending.receiver.try_recv() {
                    Ok(result) => {
                        maybe_result = Some((pending.run_id, result));
                    }
                    Err(OneshotTryRecvError::Empty) => {}
                    Err(OneshotTryRecvError::Closed) => {
                        maybe_result = Some((
                            pending.run_id,
                            Err(anyhow!("workflow task channel closed unexpectedly")),
                        ));
                    }
                }
            }

            if !progress_events.is_empty() {
                changed = true;
                for (run_id, event) in progress_events {
                    let mut progress_status = None;
                    if let Some(run) = self
                        .ai_state
                        .workflow_runs
                        .iter_mut()
                        .find(|r| r.id == run_id)
                    {
                        run.current_step = Some(event.step_id.clone());
                        run.message = format_progress_message(&event);
                        progress_status =
                            Some(format!("Workflow '{}' {}", run.workflow_name, run.message));
                    }
                    if let Some(msg) = progress_status {
                        self.set_status_message(msg);
                    }
                }
            }

            let Some((run_id, result)) = maybe_result else {
                idx += 1;
                continue;
            };

            let pending = self.ai_state.pending_workflow_runs.remove(idx);
            let _ = pending.task.is_finished();
            changed = true;

            let mut status_message = None;
            if let Some(run) = self
                .ai_state
                .workflow_runs
                .iter_mut()
                .find(|r| r.id == run_id)
            {
                run.finished_at = Some(Instant::now());
                match result {
                    Ok(ok) => {
                        run.status = crate::ai::WorkflowRunStatus::Completed;
                        run.current_step = None;
                        run.outputs = Some(ok.outputs.clone());
                        run.message = "completed".to_string();
                        status_message = Some(format!(
                            "Workflow '{}' completed (run #{})",
                            run.workflow_name, run.id
                        ));
                    }
                    Err(err) => {
                        run.status = crate::ai::WorkflowRunStatus::Failed;
                        run.current_step = None;
                        run.message = format!("failed: {}", err);
                        status_message = Some(format!(
                            "Workflow '{}' failed (run #{}): {}",
                            run.workflow_name, run.id, err
                        ));
                    }
                }
            }
            if let Some(message) = status_message {
                self.set_status_message(message);
            }
        }

        changed
    }

    pub fn workflow_status_report(&self) -> String {
        if self.ai_state.workflow_runs.is_empty() {
            return "No workflow runs yet.".to_string();
        }
        let mut lines = Vec::new();
        let keep = self.ai_state.workflow_runs.len().min(10);
        let start = self.ai_state.workflow_runs.len().saturating_sub(keep);
        for run in &self.ai_state.workflow_runs[start..] {
            let status = match run.status {
                crate::ai::WorkflowRunStatus::Running => "running",
                crate::ai::WorkflowRunStatus::Completed => "completed",
                crate::ai::WorkflowRunStatus::Failed => "failed",
            };
            let step = run
                .current_step
                .as_ref()
                .map(|s| format!(" ({s})"))
                .unwrap_or_default();
            lines.push(format!(
                "#{:>3} {:<18} {:<10} {}{}",
                run.id, run.workflow_name, status, run.message, step
            ));
        }
        lines.join("\n")
    }
}
