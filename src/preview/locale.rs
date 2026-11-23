//! Locale and time zone handling for preview metadata.
use std::time::{SystemTime, UNIX_EPOCH};

pub fn date_locale(vars: &[(String, String)]) -> Option<String> {
    ["LC_ALL", "LC_TIME", "LANG"].into_iter().find_map(|name| {
        let value = if vars.is_empty() {
            std::env::var(name).ok()
        } else {
            vars.iter()
                .find(|(key, _)| key == name || (cfg!(windows) && key.eq_ignore_ascii_case(name)))
                .map(|(_, value)| value.clone())
        };
        value.filter(|value| !value.is_empty())
    })
}

pub fn modified(time: SystemTime, locale: Option<&str>) -> Option<String> {
    let seconds = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i128::from(duration.as_secs()),
        Err(error) => {
            let duration = error.duration();
            -i128::from(duration.as_secs()) - i128::from(duration.subsec_nanos() != 0)
        }
    };
    platform::format(seconds, locale)
}

#[cfg(unix)]
mod platform {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::ptr::null_mut;

    pub fn format(seconds: i128, locale: Option<&str>) -> Option<String> {
        let seconds = libc::time_t::try_from(seconds).ok()?;
        let mut local = MaybeUninit::<libc::tm>::uninit();
        // SAFETY: both pointers refer to valid storage for their respective types.
        if unsafe { libc::localtime_r(&seconds, local.as_mut_ptr()) }.is_null() {
            return None;
        }
        // SAFETY: localtime_r succeeded and initialized local.
        let local = unsafe { local.assume_init() };
        if let Ok(name) = CString::new(locale.unwrap_or("C")) {
            // SAFETY: the name is NUL terminated and a null base creates an independent locale.
            let handle = unsafe { libc::newlocale(libc::LC_TIME_MASK, name.as_ptr(), null_mut()) };
            if !handle.is_null() {
                let mut buffer = [0u8; 256];
                // SAFETY: the buffer, time and locale are valid for the duration of the call.
                let size = unsafe {
                    libc::strftime_l(buffer.as_mut_ptr().cast(), buffer.len(), c"%x %X".as_ptr(), &local, handle)
                };
                // SAFETY: handle came from a successful newlocale and is no longer needed.
                unsafe { libc::freelocale(handle) };
                if size != 0 {
                    return Some(String::from_utf8_lossy(&buffer[..size]).into_owned());
                }
            }
        }
        Some(format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            i64::from(local.tm_year) + 1900,
            local.tm_mon + 1,
            local.tm_mday,
            local.tm_hour,
            local.tm_min,
            local.tm_sec
        ))
    }
}

#[cfg(windows)]
mod platform {
    use std::ptr::null;
    use windows_sys::Win32::{
        Foundation::{FILETIME, SYSTEMTIME},
        Globalization::{DATE_SHORTDATE, GetDateFormatEx, GetTimeFormatEx},
        System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTimeEx},
    };

    pub fn format(seconds: i128, locale: Option<&str>) -> Option<String> {
        const WINDOWS_EPOCH_OFFSET: i128 = 11_644_473_600;
        let ticks = u64::try_from((seconds + WINDOWS_EPOCH_OFFSET) * 10_000_000).ok()?;
        let filetime = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
        let mut utc = SYSTEMTIME::default();
        let mut local = SYSTEMTIME::default();
        // SAFETY: the time structures are valid; a null time zone uses the user's zone.
        if unsafe {
            FileTimeToSystemTime(&filetime, &mut utc) == 0
                || SystemTimeToTzSpecificLocalTimeEx(null(), &utc, &mut local) == 0
        } {
            return None;
        }
        let formatting = |name| {
            let mut date = [0u16; 128];
            let mut time = [0u16; 128];
            // SAFETY: the locale pointer is null or NUL terminated and the buffers are writable.
            let (date_len, time_len) = unsafe {
                (
                    GetDateFormatEx(name, DATE_SHORTDATE, &local, null(), date.as_mut_ptr(), date.len() as i32, null()),
                    GetTimeFormatEx(name, 0, &local, null(), time.as_mut_ptr(), time.len() as i32),
                )
            };
            (date_len > 1 && time_len > 1).then(|| {
                format!(
                    "{} {}",
                    String::from_utf16_lossy(&date[..date_len as usize - 1]),
                    String::from_utf16_lossy(&time[..time_len as usize - 1])
                )
            })
        };
        let explicit = locale.filter(|name| !name.contains('\0')).and_then(|name| {
            let normalized = name.split(['.', '@']).next()?.replace('_', "-");
            if normalized.is_empty() {
                return None;
            }
            let name: Vec<_> = normalized.encode_utf16().chain([0]).collect();
            formatting(name.as_ptr())
        });
        explicit.or_else(|| formatting(null())).or_else(|| {
            Some(format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond
            ))
        })
    }
}
