//! Client file transfer: `Download`, `DownloadFromStream`, `Upload`, `UploadIntoStream` (global and
//! `File.` forms). In BC the service tier hands the file action to the session's client
//! (Ncl NavFile.InternalDownload / ALUploadIntoStream → IClientCallback.DownloadFileAction /
//! UploadFileAction), so the answer is the client's:
//!
//! - Test runs: Microsoft's CI drives the tests through BcContainerHelper's ClientContext, a
//!   logical client without a file handler, so the web server's NavWebFileDialogProvider answers:
//!   FileSave offers the file as a download URL and returns true with the requested file name
//!   (Download / DownloadFromStream succeed, ToFile unchanged); FileOpen shows a FileUploadForm that
//!   nobody fills in and returns false (Upload / UploadIntoStream return false, no error).
//! - Sessions with a client (bc-nst web client sessions): the same answers (bc-rust does not pass
//!   the file to the browser yet).
//! - Sessions without a client (background, API): an error, in bc-rust's own words [src:X11].

use crate::interp::*;
use crate::value::*;
use al_syntax::Expr;

pub fn is_transfer(l: &str) -> bool {
    matches!(l, "download" | "downloadfromstream" | "upload" | "uploadintostream")
}

impl<'r> Session<'r> {
    /// `l` (lower-case) is one of [`is_transfer`].
    pub fn file_transfer(&mut self, f: &mut Frame, l: &str, args: &[Expr]) -> R<Val> {
        let mut a = Vec::with_capacity(args.len());
        for e in args {
            a.push(self.eval(f, e)?);
        }
        let download = l.starts_with("download");
        let has_client = self.in_test() || self.gui_allowed || self.ui.is_some();
        if !has_client {
            // the file name argument of each method (Learn: Download, DownloadFromStream, Upload,
            // UploadIntoStream); own wording [src:X11]
            let file = match l {
                "download" | "downloadfromstream" => a.get(4),
                _ if a.len() >= 4 => a.get(3),
                _ => None,
            }
            .map(|v| v.to_text())
            .unwrap_or_default();
            let what = if download { "downloaded" } else { "uploaded" };
            return err(format!("This session has no client, so the file '{file}' cannot be {what}."));
        }
        Ok(Val::Bool(download))
    }
}
