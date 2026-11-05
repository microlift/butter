//! What the session's Wayland compositor offers, asked over its socket.
//!
//! Rosemary's windows are SDL's, and SDL connects to the compositor on its
//! own, whatever GDK uses for Butter. So this asks the compositor directly,
//! the way a fresh client would: it lists the globals and goes.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

/// The global a compositor advertises when it draws titlebars for the
/// windows that ask it to (server-side decorations).
const DECORATION_MANAGER: &str = "zxdg_decoration_manager_v1";

/// Whether the compositor draws titlebars itself. `None` when this is not a
/// Wayland session, or the compositor could not be asked. Asked once; it
/// blocks for up to a couple of seconds, so call it off the main thread.
pub fn server_decorations() -> Option<bool> {
    static ANSWER: OnceLock<Option<bool>> = OnceLock::new();
    *ANSWER.get_or_init(|| {
        let globals = globals_at(&socket_path()?)
            .map_err(|e| eprintln!("butter: could not ask the Wayland compositor: {e}"))
            .ok()?;
        Some(globals.iter().any(|g| g == DECORATION_MANAGER))
    })
}

/// The compositor's socket, as libwayland finds it. Without
/// `WAYLAND_DISPLAY` this is taken not to be a Wayland session.
fn socket_path() -> Option<PathBuf> {
    let display = PathBuf::from(std::env::var_os("WAYLAND_DISPLAY").filter(|d| !d.is_empty())?);
    if display.is_absolute() {
        return Some(display);
    }
    Some(PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join(display))
}

// Object ids: the display is always 1; the client picks the others.
const DISPLAY: u32 = 1;
const REGISTRY: u32 = 2;
const CALLBACK: u32 = 3;

/// The interfaces of the globals the compositor at `path` advertises.
fn globals_at(path: &Path) -> io::Result<Vec<String>> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    // wl_display.get_registry, then wl_display.sync: the registry's globals
    // all arrive before the sync's callback is done.
    let mut request = Vec::with_capacity(24);
    message(&mut request, DISPLAY, 1, REGISTRY);
    message(&mut request, DISPLAY, 0, CALLBACK);
    stream.write_all(&request)?;

    let mut globals = Vec::new();
    let mut pending = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        pending.extend_from_slice(&chunk[..n]);
        let (used, done) = read_events(&pending, &mut globals)?;
        if done {
            return Ok(globals);
        }
        pending.drain(..used);
    }
}

/// Append a request with one `new_id` argument to `out`.
fn message(out: &mut Vec<u8>, object: u32, opcode: u16, new_id: u32) {
    out.extend_from_slice(&object.to_ne_bytes());
    out.extend_from_slice(&((12u32 << 16) | u32::from(opcode)).to_ne_bytes());
    out.extend_from_slice(&new_id.to_ne_bytes());
}

fn word(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn malformed(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

/// Read the whole events at the start of `bytes`, adding each global's
/// interface to `globals`. Returns how many bytes were read, and whether
/// the sync's callback is done.
fn read_events(bytes: &[u8], globals: &mut Vec<String>) -> io::Result<(usize, bool)> {
    let mut at = 0;
    while let (Some(object), Some(header)) = (word(bytes, at), word(bytes, at + 4)) {
        let size = (header >> 16) as usize;
        let opcode = header & 0xffff;
        if size < 8 || !size.is_multiple_of(4) {
            return Err(malformed("an event with a bad size"));
        }
        let Some(body) = bytes.get(at + 8..at + size) else {
            break;
        };
        match (object, opcode) {
            (DISPLAY, 0) => return Err(malformed("the compositor reported an error")),
            // wl_registry.global: name, interface, version.
            (REGISTRY, 0) => {
                let len = word(body, 4).ok_or_else(|| malformed("a short global"))? as usize;
                let name = body
                    .get(8..8 + len)
                    .ok_or_else(|| malformed("a short global"))?;
                // The length counts the terminating NUL.
                let name = name.strip_suffix(&[0]).unwrap_or(name);
                globals.push(String::from_utf8_lossy(name).into_owned());
            }
            // wl_callback.done
            (CALLBACK, 0) => return Ok((at + size, true)),
            _ => {}
        }
        at += size;
    }
    Ok((at, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(out: &mut Vec<u8>, object: u32, opcode: u32, body: &[u8]) {
        let size = 8 + body.len() as u32;
        out.extend_from_slice(&object.to_ne_bytes());
        out.extend_from_slice(&((size << 16) | opcode).to_ne_bytes());
        out.extend_from_slice(body);
    }

    fn global(out: &mut Vec<u8>, name: u32, interface: &str) {
        let mut body = name.to_ne_bytes().to_vec();
        body.extend_from_slice(&(interface.len() as u32 + 1).to_ne_bytes());
        body.extend_from_slice(interface.as_bytes());
        body.push(0);
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
        body.extend_from_slice(&1u32.to_ne_bytes());
        event(out, REGISTRY, 0, &body);
    }

    fn compositor(interfaces: &[&str]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (i, interface) in interfaces.iter().enumerate() {
            global(&mut bytes, i as u32 + 1, interface);
        }
        // wl_display.delete_id, which is not ours to read.
        event(&mut bytes, DISPLAY, 1, &CALLBACK.to_ne_bytes());
        event(&mut bytes, CALLBACK, 0, &0u32.to_ne_bytes());
        bytes
    }

    #[test]
    fn reads_globals_until_done() {
        let bytes = compositor(&["wl_compositor", "xdg_wm_base", DECORATION_MANAGER]);
        let mut globals = Vec::new();
        assert_eq!(
            read_events(&bytes, &mut globals).unwrap(),
            (bytes.len(), true)
        );
        assert_eq!(
            globals,
            ["wl_compositor", "xdg_wm_base", DECORATION_MANAGER]
        );
    }

    #[test]
    fn waits_for_whole_events() {
        let bytes = compositor(&["wl_seat"]);
        let mut globals = Vec::new();
        let (used, done) = read_events(&bytes[..10], &mut globals).unwrap();
        assert_eq!((used, done), (0, false));
        assert!(globals.is_empty());
    }

    #[test]
    fn rejects_errors() {
        let mut bytes = Vec::new();
        event(&mut bytes, DISPLAY, 0, &[0; 12]);
        assert!(read_events(&bytes, &mut Vec::new()).is_err());
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&REGISTRY.to_ne_bytes());
        bytes.extend_from_slice(&(4u32 << 16).to_ne_bytes());
        assert!(read_events(&bytes, &mut Vec::new()).is_err());
    }

    /// A compositor that answers in small pieces, as a socket may.
    #[test]
    fn asks_a_compositor() {
        let dir = std::env::temp_dir().join(format!("butter-wayland-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wayland-test");
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut client, _) = listener.accept().unwrap();
            let mut request = [0u8; 24];
            client.read_exact(&mut request).unwrap();
            let mut expected = Vec::new();
            message(&mut expected, DISPLAY, 1, REGISTRY);
            message(&mut expected, DISPLAY, 0, CALLBACK);
            assert_eq!(request.as_slice(), expected);
            for piece in compositor(&["wl_compositor", "wl_shm"]).chunks(7) {
                client.write_all(piece).unwrap();
                client.flush().unwrap();
            }
        });
        let globals = globals_at(&path).unwrap();
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(globals, ["wl_compositor", "wl_shm"]);
    }
}
