use alloc::format;
use alloc::string::String;

const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

pub(super) fn iec_bytes(s: u64) -> String {
    humanize_bytes(s, 1024.0)
}

#[inline(always)]
fn humanize_bytes(s: u64, base: f64) -> String {
    if s < 10 {
        return format!("{} B", s);
    }

    let sf64 = s as f64;
    let e = sf64.log(base).floor() as usize;
    let suffix = UNITS[e];
    let val = (sf64 / base.powf(e as f64) * 10f64 + 0.5).floor() / 10f64;
    if val < 10f64 {
        format!("{:.1} {}", val, suffix)
    } else {
        format!("{:.0} {}", val, suffix)
    }
}
