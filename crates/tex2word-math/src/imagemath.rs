//! Image fallback for math (port of `mathml/imagemath.py`): render a formula
//! with a real TeX engine (`latex` + `dvipng`) when the direct OMML path
//! can't. (The reference's matplotlib backend is an optional Python extra.)

use std::path::PathBuf;
use std::process::Command;

use crate::cascade::ImageMathRenderer;

pub const DEFAULT_DPI: u32 = 200;

/// `shutil.which(name)`.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| {
        std::fs::metadata(p).is_ok_and(|m| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                m.is_file() && m.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                m.is_file()
            }
        })
    })
}

/// A scratch directory removed on drop (`tempfile.TemporaryDirectory`).
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<TempDir> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "tex2word-math-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p)?;
        Ok(TempDir(p))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Render math to PNG via `latex` -> dvi -> `dvipng`.
pub struct DvipngMathRenderer {
    pub dpi: u32,
}

const PREAMBLE: &str = "\\documentclass[border=2pt]{standalone}\n\\usepackage{amsmath,amssymb,amsfonts}\n\\begin{document}\n";

impl DvipngMathRenderer {
    pub fn available() -> bool {
        which("latex").is_some() && which("dvipng").is_some()
    }

    fn try_render(&self, latex: &str, display: bool) -> Option<(Vec<u8>, String)> {
        let body = if display { format!("\\[{latex}\\]") } else { format!("${latex}$") };
        let doc = format!("{PREAMBLE}{body}\n\\end{{document}}\n");
        let td = TempDir::new().ok()?;
        std::fs::write(td.0.join("m.tex"), doc).ok()?;
        let status = Command::new("latex")
            .args(["-interaction=nonstopmode", "-halt-on-error", "m.tex"])
            .current_dir(&td.0)
            .output()
            .ok()?;
        let dvi = td.0.join("m.dvi");
        if !status.status.success() || !dvi.exists() {
            return None;
        }
        let png = td.0.join("m.png");
        let out = Command::new("dvipng")
            .args(["-D", &self.dpi.to_string(), "-T", "tight", "-bg", "Transparent", "-o"])
            .arg(&png)
            .arg(&dvi)
            .current_dir(&td.0)
            .output()
            .ok()?;
        if !out.status.success() || !png.exists() {
            return None;
        }
        Some((std::fs::read(&png).ok()?, "png".into()))
    }
}

impl ImageMathRenderer for DvipngMathRenderer {
    fn render(&mut self, latex: &str, display: bool) -> Option<(Vec<u8>, String)> {
        if !Self::available() {
            return None;
        }
        self.try_render(latex, display)
    }
}

/// The best available math-image renderer, or `None`.
pub fn default_renderer(dpi: u32) -> Option<Box<dyn ImageMathRenderer>> {
    if DvipngMathRenderer::available() {
        return Some(Box::new(DvipngMathRenderer { dpi }));
    }
    None
}
