//! Live checks for the Linux input/display modules. Only harmless events:
//!   input_probe outputs
//!   input_probe keymap [keysym...]        print the generated xkb keymap (with slots bound)
//!   input_probe move OUTPUT W H X Y       absolute motion in OUTPUT pixels
//!   input_probe scroll OUTPUT W H X Y     move, then wheel down/up and a finger scroll down/up
//!   input_probe shift                     Shift_L press/release
//!   input_probe numlock SECS              wait SECS, Num_Lock tap, wait SECS, tap again, wait SECS
//!   input_probe layout INPUT_SOURCE_ID SECS  upload the keymap for a macOS input source, wait SECS

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use std::time::Duration;

    use wayshare::display::{DisplayManager, hyprland::Hyprland};
    use wayshare::input::keyboard::{KeyboardConfig, KeyboardEngine};
    use wayshare::input::wayland::WaylandInput;
    use wayshare::input::{InputSink, PointerConfig, ScrollEvent};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| -> anyhow::Result<u32> {
        Ok(args
            .get(i)
            .ok_or_else(|| anyhow::anyhow!("missing arg {i}"))?
            .parse()?)
    };
    let sleep = |ms| std::thread::sleep(Duration::from_millis(ms));
    let connect = |output: Option<&str>, w, h| {
        WaylandInput::connect(
            output,
            w,
            h,
            KeyboardConfig::default(),
            PointerConfig::default(),
        )
    };
    match args.first().map(String::as_str) {
        Some("outputs") => {
            for o in Hyprland::from_env()?.outputs()? {
                println!("{o:?}");
            }
        }
        Some("keymap") => {
            let mut e = KeyboardEngine::new(KeyboardConfig::default());
            for ks in &args[1..] {
                let ks = u32::from_str_radix(ks.trim_start_matches("0x"), 16)?;
                e.key(true, ks, None);
                e.key(false, ks, None);
            }
            print!("{}", e.keymap());
        }
        Some(cmd @ ("move" | "scroll")) => {
            let mut inp = connect(Some(&args[1]), arg(2)?, arg(3)?)?;
            let (x, y) = (arg(4)? as i32, arg(5)? as i32);
            inp.pointer(x, y, 0)?;
            sleep(100);
            if cmd == "scroll" {
                inp.pointer(x, y, 16)?;
                sleep(100);
                inp.pointer(x, y, 8)?;
                sleep(100);
                let mut ev = ScrollEvent {
                    flags: 2,
                    point_y: -5,
                    scroll_phase: 1,
                    ..Default::default()
                };
                inp.scroll(&ev)?;
                ev.scroll_phase = 2;
                inp.scroll(&ev)?;
                ev.point_y = 10;
                inp.scroll(&ev)?;
                ev.point_y = 0;
                ev.scroll_phase = 4;
                inp.scroll(&ev)?;
                sleep(100);
            }
            inp.sync()?;
        }
        Some("shift") => {
            let mut inp = connect(None, 1, 1)?;
            sleep(200);
            inp.key(true, 0xffe1, None)?;
            sleep(50);
            inp.key(false, 0xffe1, None)?;
            inp.sync()?;
            sleep(200);
        }
        Some("numlock") => {
            let mut inp = connect(None, 1, 1)?;
            let hold = Duration::from_secs(arg(1)? as u64);
            inp.sync()?;
            std::thread::sleep(hold);
            inp.key(true, 0xff7f, None)?;
            inp.key(false, 0xff7f, None)?;
            inp.sync()?;
            std::thread::sleep(hold);
            inp.key(true, 0xff7f, None)?;
            inp.key(false, 0xff7f, None)?;
            inp.sync()?;
            std::thread::sleep(hold);
        }
        Some("layout") => {
            let mut inp = connect(None, 1, 1)?;
            inp.set_input_source(&args[1])?;
            inp.sync()?;
            std::thread::sleep(Duration::from_secs(arg(2)? as u64));
        }
        _ => anyhow::bail!("usage: see source"),
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Linux only");
}
