//! Opt-in native child-webview smoke test used by local and CI GUI runners.

use ovim_core::browser::{
    BrowserAction, BrowserClient, BrowserCommand, BrowserResponse, BrowserSnapshot,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use super::host::BrowserHost;
use super::state::GuiBrowserKeyMode;

const SMOKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) async fn run_native_browser_smoke(
    client: BrowserClient,
    host: BrowserHost,
) -> Result<(), String> {
    let origin = start_loopback_server()?;
    let first = start_session(&client, format!("{origin}/first")).await?;
    let mut opened = vec![first.clone()];
    let result = async {
        wait_until_loaded(&client, &first).await?;
        let initial = snapshot(&client, &first).await?;
        let input = initial
            .elements
            .iter()
            .find(|element| element.name == "Draft" && element.role == "textbox")
            .ok_or_else(|| "Native smoke page input was not discoverable".to_string())?;
        act(
            &client,
            &initial,
            BrowserAction::Type {
                element: input.reference.clone(),
                text: "kept across tabs".into(),
            },
        )
        .await?;
        wait_for_key_mode(&host, &first, GuiBrowserKeyMode::Insert).await?;

        let typed = snapshot(&client, &first).await?;
        let retained_input = typed
            .elements
            .iter()
            .find(|element| element.name == "Draft" && element.role == "textbox")
            .ok_or_else(|| "Native smoke page input disappeared after typing".to_string())?;
        if retained_input.value.as_deref() != Some("kept across tabs") {
            return Err("Native browser did not retain the typed input value".into());
        }
        act(&client, &typed, BrowserAction::Scroll { delta_y: 900 }).await?;
        let scrolled = snapshot(&client, &first).await?;
        if scrolled.viewport.scroll_y <= 0 {
            return Err("Native browser did not apply the scroll action".into());
        }

        let second = start_session(&client, format!("{origin}/second")).await?;
        opened.push(second.clone());
        wait_until_loaded(&client, &second).await?;
        expect_session(
            client
                .execute(BrowserCommand::Show {
                    session_id: first.clone(),
                })
                .await
                .map_err(|error| error.message)?,
            "show first tab",
        )?;

        let restored = snapshot(&client, &first).await?;
        let restored_input = restored
            .elements
            .iter()
            .find(|element| element.name == "Draft" && element.role == "textbox")
            .ok_or_else(|| "First tab input disappeared after switching tabs".to_string())?;
        if restored_input.value.as_deref() != Some("kept across tabs") {
            return Err("Switching tabs lost the first tab's form state".into());
        }
        if restored.viewport.scroll_y <= 0 {
            return Err("Switching tabs lost the first tab's scroll position".into());
        }
        Ok(())
    }
    .await;

    for session_id in opened.into_iter().rev() {
        let _ = client.execute(BrowserCommand::Close { session_id }).await;
    }
    result
}

async fn start_session(client: &BrowserClient, url: String) -> Result<String, String> {
    let response = client
        .execute(BrowserCommand::Start { url: Some(url) })
        .await
        .map_err(|error| error.message)?;
    Ok(expect_session(response, "start tab")?.session_id)
}

fn expect_session(
    response: BrowserResponse,
    operation: &str,
) -> Result<ovim_core::browser::BrowserSession, String> {
    match response {
        BrowserResponse::Session(session) => Ok(session),
        _ => Err(format!(
            "Native browser smoke {operation} returned the wrong response"
        )),
    }
}

async fn wait_until_loaded(client: &BrowserClient, session_id: &str) -> Result<(), String> {
    let deadline = Instant::now() + SMOKE_TIMEOUT;
    while Instant::now() < deadline {
        let response = client
            .execute(BrowserCommand::List)
            .await
            .map_err(|error| error.message)?;
        let BrowserResponse::Sessions(sessions) = response else {
            return Err("Native browser smoke list returned the wrong response".into());
        };
        if sessions
            .iter()
            .find(|session| session.session_id == session_id)
            .is_some_and(|session| !session.loading && !session.url.is_empty())
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Err(format!(
        "Native browser session {session_id} did not finish loading"
    ))
}

async fn wait_for_key_mode(
    host: &BrowserHost,
    session_id: &str,
    expected: GuiBrowserKeyMode,
) -> Result<(), String> {
    let deadline = Instant::now() + SMOKE_TIMEOUT;
    while Instant::now() < deadline {
        if host
            .state()
            .sessions
            .iter()
            .find(|session| session.session_id == session_id)
            .is_some_and(|session| session.key_mode == expected)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Err(format!(
        "Native browser session {session_id} did not enter {expected:?} mode"
    ))
}

async fn snapshot(client: &BrowserClient, session_id: &str) -> Result<BrowserSnapshot, String> {
    match client
        .execute(BrowserCommand::Snapshot {
            session_id: session_id.to_string(),
        })
        .await
        .map_err(|error| error.message)?
    {
        BrowserResponse::Snapshot(snapshot) => Ok(snapshot),
        _ => Err("Native browser smoke snapshot returned the wrong response".into()),
    }
}

async fn act(
    client: &BrowserClient,
    snapshot: &BrowserSnapshot,
    action: BrowserAction,
) -> Result<(), String> {
    expect_session(
        client
            .execute(BrowserCommand::Act {
                session_id: snapshot.session.session_id.clone(),
                document_id: snapshot.session.document_id,
                snapshot_id: snapshot.snapshot_id,
                action,
            })
            .await
            .map_err(|error| error.message)?,
        "act",
    )?;
    Ok(())
}

fn start_loopback_server() -> Result<String, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("Could not bind native browser smoke server: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("Could not read native browser smoke address: {error}"))?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            serve_smoke_page(stream);
        }
    });
    Ok(format!("http://{address}"))
}

fn serve_smoke_page(mut stream: TcpStream) {
    let mut request = [0_u8; 2048];
    let read = stream.read(&mut request).unwrap_or(0);
    let path = std::str::from_utf8(&request[..read])
        .ok()
        .and_then(|request| request.lines().next())
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let title = if path.starts_with("/second") {
        "Second tab"
    } else {
        "First tab"
    };
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>{title}</title>\
         <label>Draft <input aria-label=Draft></label>\
         <a href=/second>Second page</a>\
         <main style='height:2400px;padding-top:20px'>Native browser smoke</main>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}
