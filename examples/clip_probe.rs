//! Live checks for the clipboard backend. Never prints clipboard contents.
//!   clip_probe globals       data-control globals the compositor offers
//!   clip_probe watch SECS    selection changes: serial and offered MIME types only
//!   clip_probe offer SECS    own the selection with synthetic text+html for SECS, then exit
//!   clip_probe promise SECS  own the selection with promised text; supply it when pasted
//!   clip_probe read          read the selection and print MIME sizes (only for synthetic data)

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use std::time::{Duration, Instant};

    use wayshare::clipboard::wayland::{WaylandClipboard, data_control_globals};
    use wayshare::clipboard::{ClipContent, ClipboardBackend, mime};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let secs =
        || -> anyhow::Result<u64> { Ok(args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(5)) };
    match args.first().map(String::as_str) {
        Some("globals") => println!("{:?}", data_control_globals()?),
        Some("watch") => {
            let cb = WaylandClipboard::spawn()?;
            let end = Instant::now() + Duration::from_secs(secs()?);
            let mut last = u64::MAX;
            while Instant::now() < end {
                let s = cb.serial();
                if s != last {
                    println!("serial {s} offered {:?}", cb.offered());
                    last = s;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Some("offer") => {
            let cb = WaylandClipboard::spawn()?;
            std::thread::sleep(Duration::from_millis(300));
            cb.set(ClipContent(vec![
                (mime::HTML.into(), b"<b>wayshare synthetic</b>".to_vec()),
                (
                    mime::TEXT.into(),
                    b"wayshare synthetic \xe2\x98\x83".to_vec(),
                ),
            ]))?;
            std::thread::sleep(Duration::from_millis(300));
            println!("own selection, offered {:?}", cb.offered());
            std::thread::sleep(Duration::from_secs(secs()?));
        }
        Some("promise") => {
            let cb = WaylandClipboard::spawn()?;
            std::thread::sleep(Duration::from_millis(300));
            cb.set_promised(vec![mime::TEXT])?;
            println!("promised text; waiting for a paste");
            let end = Instant::now() + Duration::from_secs(secs()?);
            while Instant::now() < end {
                if cb.take_data_request() {
                    println!("data requested; fulfilling");
                    cb.set(ClipContent::text("wayshare promised synthetic"))?;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Some("read") => {
            let cb = WaylandClipboard::spawn()?;
            std::thread::sleep(Duration::from_millis(300));
            println!("offered {:?}", cb.offered());
            println!("sizes {:?}", cb.read()?.summary());
        }
        _ => anyhow::bail!("usage: see source"),
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Linux only");
}
