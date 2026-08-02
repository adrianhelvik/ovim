//! Asynchronous display-math rendering for AI chat.
//!
//! MathJax is bundled and produces SVG; resvg rasterizes it because terminal
//! graphics protocols consume pixel images. The UI only polls this cache and
//! never waits for the renderer worker.

use mathjax_svg_rs::{HorizontalAlign, Options};
use std::{
    collections::HashMap,
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{mpsc, Arc, Mutex, OnceLock},
};

const FONT_SIZE_PX: f64 = 22.0;
const CELL_WIDTH_PX: f32 = 10.0;
const CELL_HEIGHT_PX: f32 = 20.0;
/// Display footprint is independent of the 2x raster resolution below.
const DISPLAY_SCALE: f32 = 1.3;
const MIN_DISPLAY_ROWS: f32 = 2.0;
const RASTER_SCALE: f32 = 2.0;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MathKey {
    tex: String,
    max_columns: u16,
    color: [u8; 3],
}

#[derive(Debug, Clone)]
pub(crate) struct RenderedMath {
    pub path: PathBuf,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone)]
pub(crate) enum MathRenderStatus {
    Ready(RenderedMath),
    Pending,
    Failed,
}

#[derive(Debug, Clone)]
enum CacheEntry {
    Pending,
    Ready(RenderedMath),
    Failed,
}

struct MathRenderCache {
    entries: Arc<Mutex<HashMap<MathKey, CacheEntry>>>,
    requests: mpsc::Sender<MathKey>,
}

impl MathRenderCache {
    fn new() -> Self {
        let entries = Arc::new(Mutex::new(HashMap::new()));
        let worker_entries = Arc::clone(&entries);
        let (requests, receiver) = mpsc::channel::<MathKey>();
        std::thread::Builder::new()
            .name("ovim-math-renderer".to_string())
            .spawn(move || {
                // Initializing the embedded JavaScript runtime is expensive in
                // debug builds. Do it while Ovim starts rather than when the
                // first equation becomes visible.
                let _ = render_to_png(&MathKey {
                    tex: "x".to_string(),
                    max_columns: 4,
                    color: [200, 208, 220],
                });
                while let Ok(key) = receiver.recv() {
                    let entry = match render_to_png(&key) {
                        Ok(rendered) => CacheEntry::Ready(rendered),
                        Err(_) => CacheEntry::Failed,
                    };
                    worker_entries
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(key, entry);
                }
            })
            .expect("failed to start display-math renderer");
        Self { entries, requests }
    }

    fn get_or_request(&self, key: MathKey) -> MathRenderStatus {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match entries.get(&key) {
            Some(CacheEntry::Ready(rendered)) => MathRenderStatus::Ready(rendered.clone()),
            Some(CacheEntry::Pending) => MathRenderStatus::Pending,
            Some(CacheEntry::Failed) => MathRenderStatus::Failed,
            None => {
                entries.insert(key.clone(), CacheEntry::Pending);
                if self.requests.send(key.clone()).is_err() {
                    entries.insert(key, CacheEntry::Failed);
                    MathRenderStatus::Failed
                } else {
                    MathRenderStatus::Pending
                }
            }
        }
    }
}

fn cache() -> &'static MathRenderCache {
    static CACHE: OnceLock<MathRenderCache> = OnceLock::new();
    CACHE.get_or_init(MathRenderCache::new)
}

pub(crate) fn start_display_math_renderer() {
    let _ = cache();
}

pub(crate) fn request_display_math(
    tex: &str,
    max_columns: u16,
    color: [u8; 3],
) -> MathRenderStatus {
    if tex.trim().is_empty() || max_columns == 0 {
        return MathRenderStatus::Failed;
    }
    cache().get_or_request(MathKey {
        tex: tex.trim().to_string(),
        max_columns,
        color,
    })
}

fn render_to_png(key: &MathKey) -> Result<RenderedMath, String> {
    let options = Options {
        font_size: FONT_SIZE_PX,
        horizontal_align: HorizontalAlign::Left,
    };
    let svg = mathjax_svg_rs::render_tex(&key.tex, &options)?;
    let color = format!(
        "#{:02x}{:02x}{:02x}",
        key.color[0], key.color[1], key.color[2]
    );
    let svg = svg.replace("currentColor", &color);
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())
        .map_err(|error| format!("invalid MathJax SVG: {error}"))?;

    let natural = tree.size();
    if natural.width() <= 0.0 || natural.height() <= 0.0 {
        return Err("MathJax produced an empty image".to_string());
    }

    let max_logical_width = key.max_columns as f32 * CELL_WIDTH_PX;
    let minimum_height_scale = MIN_DISPLAY_ROWS * CELL_HEIGHT_PX / natural.height();
    let desired_scale = DISPLAY_SCALE.max(minimum_height_scale);
    let logical_scale = (max_logical_width / natural.width()).min(desired_scale);
    let logical_width = (natural.width() * logical_scale).ceil().max(1.0);
    let logical_height = (natural.height() * logical_scale).ceil().max(1.0);
    let pixel_width = (logical_width * RASTER_SCALE).ceil() as u32;
    let pixel_height = (logical_height * RASTER_SCALE).ceil() as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pixel_width, pixel_height)
        .ok_or_else(|| "display-math image is too large".to_string())?;
    let transform = resvg::tiny_skia::Transform::from_scale(
        logical_scale * RASTER_SCALE,
        logical_scale * RASTER_SCALE,
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    let directory = std::env::temp_dir().join("ovim-display-math");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create math cache: {error}"))?;
    let path = directory.join(format!("{:016x}.png", hasher.finish()));
    pixmap
        .save_png(&path)
        .map_err(|error| format!("could not save math image: {error}"))?;

    Ok(RenderedMath {
        path,
        width: ((logical_width / CELL_WIDTH_PX).ceil() as u16).clamp(1, key.max_columns),
        height: ((logical_height / CELL_HEIGHT_PX).ceil() as u16).max(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_fraction_to_nonempty_png() {
        let key = MathKey {
            tex: r"F(R) \le \frac{2}{1+m_1} F(2R)".to_string(),
            max_columns: 60,
            color: [200, 208, 220],
        };
        let rendered = render_to_png(&key).expect("render math");
        let metadata = fs::metadata(&rendered.path).expect("rendered PNG");
        assert!(metadata.len() > 100);
        assert!((1..=60).contains(&rendered.width));
        assert!(rendered.height >= 2);
    }

    #[test]
    fn renders_matrix_display_math() {
        let key = MathKey {
            tex: r"u(x,y,z,t)=\begin{pmatrix}u_1\\u_2\\u_3\end{pmatrix}".to_string(),
            max_columns: 36,
            color: [200, 208, 220],
        };
        let rendered = render_to_png(&key).expect("render matrix");
        assert!(rendered.path.is_file());
    }

    #[test]
    fn asynchronous_request_eventually_returns_cached_image() {
        let tex = r"u(x,y,z,t)=\begin{pmatrix}u_1\\u_2\\u_3\end{pmatrix}";
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match request_display_math(tex, 50, [200, 208, 220]) {
                MathRenderStatus::Ready(rendered) => {
                    assert!(rendered.path.is_file());
                    assert!((1..=50).contains(&rendered.width));
                    break;
                }
                MathRenderStatus::Pending if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                MathRenderStatus::Pending => panic!("display-math worker timed out"),
                MathRenderStatus::Failed => panic!("display-math worker failed"),
            }
        }
    }
}
