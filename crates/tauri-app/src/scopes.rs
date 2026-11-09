//! The Scopes panel: waveform, parade, vectorscope and histogram of the
//! Viewer's picture (see `reelforge_engine::scopes`).

use crate::{ed, Res};
use reelforge_engine::scopes::{render, ScopeOptions};
use std::cell::RefCell;
use std::rc::Rc;
use tauri::ipc::Response;

thread_local! {
    /// The Viewer's last picture read back from the GPU: its `picture_rev`,
    /// pixels and size (shared by the Viewer and the scopes, so it's read once).
    static LAST: RefCell<Option<(u64, Rc<Vec<u8>>, [u32; 2])>> = const { RefCell::new(None) };
}

/// The Viewer's picture at `rev`, read back once.
pub(crate) fn picture(e: &reelforge_engine::editor::Editor) -> Option<(Rc<Vec<u8>>, [u32; 2])> {
    let rev = e.preview.picture_rev;
    if let Some((r, px, size)) = LAST.with(|l| l.borrow().clone()) {
        if r == rev {
            return Some((px, size));
        }
    }
    let (px, size) = e.preview.read_output()?;
    let px = Rc::new(px);
    LAST.with(|l| *l.borrow_mut() = Some((rev, px.clone(), size)));
    Some((px, size))
}

/// One scope of the Viewer's picture, unless the picture is still `have`
/// (its `picture_rev`). Bytes: kind (0 = unchanged or none, 1 = RGBA), then
/// width and height (u32, little-endian), the picture's `picture_rev` (u64),
/// then the pixels.
#[tauri::command]
pub fn scope(options: ScopeOptions, have: i64) -> Res<Response> {
    ed(|e| {
        let rev = e.preview.picture_rev;
        if rev as i64 == have {
            return Response::new(vec![0u8]);
        }
        let Some((px, [w, h])) = picture(e) else { return Response::new(vec![0u8]) };
        let img = render(&px, w, h, &options);
        let (ow, oh) = (options.width.clamp(16, 2048), options.height.clamp(16, 2048));
        let mut out = Vec::with_capacity(17 + img.len());
        out.push(1);
        out.extend(ow.to_le_bytes());
        out.extend(oh.to_le_bytes());
        out.extend(rev.to_le_bytes());
        out.extend(img);
        Response::new(out)
    })
}
