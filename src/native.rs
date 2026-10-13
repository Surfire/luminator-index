use anyhow::{Context, Result, bail, ensure};
use std::collections::HashMap;
use std::{
    ffi::{CStr, CString, c_char, c_void},
    fs::OpenOptions,
    mem::MaybeUninit,
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
};

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct Span {
    pub start: u32,
    pub len: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Dirent {
    name: *const c_char,
    name_len: u32,
    pub kind: u32,
    flags: u32,
    pub dev: i32,
    error: i32,
    pub ino: u64,
    pub size: u64,
    pub mtime: i64,
}

unsafe extern "C" {
    fn sift_read_dir(fd: i32, scratch: *mut c_void, size: usize, out: *mut Dirent, cap: i32)
    -> i32;
    pub(crate) fn lumi_search(
        blob: *const *const u8,
        names: *const *const Span,
        full: *const *const Span,
        initials: *const *const Span,
        kinds: *const *const u8,
        signatures: *const *const u64,
        required: u64,
        candidates: *const u32,
        start: u32,
        end: u32,
        needle: *const u8,
        len: usize,
        ascii: i32,
        mode: i32,
        ids: *mut u32,
        scores: *mut i32,
    ) -> usize;
    fn lumi_transliterate(input: *const u8, len: usize, output: *mut c_char, cap: usize) -> i32;
    fn lumi_watch_start(
        root: *const c_char,
        since: u64,
        cb: extern "C" fn(*mut c_void, *const c_char, u32, u64),
        ctx: *mut c_void,
    ) -> *mut c_void;
    fn lumi_watch_stop(w: *mut c_void);
    fn lumi_event_id() -> u64;
    fn lumi_volume_uuid(dev: i32, output: *mut c_char, capacity: usize) -> i32;
    fn lumi_dataless(flags: u32) -> i32;
    fn lumi_footprint() -> u64;
    fn lumi_footprint_peak() -> u64;
}

pub fn footprint() -> u64 {
    unsafe { lumi_footprint() }
}
pub fn footprint_peak() -> u64 {
    unsafe { lumi_footprint_peak() }
}
pub fn event_id() -> u64 {
    unsafe { lumi_event_id() }
}
pub fn volume_uuid(dev: i32) -> Result<String> {
    let mut output = [0i8; 128];
    ensure!(
        unsafe { lumi_volume_uuid(dev, output.as_mut_ptr(), output.len()) } != 0,
        "cannot read volume event UUID"
    );
    Ok(unsafe { CStr::from_ptr(output.as_ptr()) }
        .to_str()?
        .to_owned())
}

pub(crate) fn read_dir(
    path: &Path,
    mut visit: impl FnMut(&str, Dirent, bool) -> Result<()>,
) -> Result<u64> {
    let dir = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("cannot enumerate {}", path.display()))?;
    let mut scratch = vec![0u64; 8192];
    let mut entries = vec![MaybeUninit::<Dirent>::uninit(); 1024];
    let mut skipped = 0;
    loop {
        let n = unsafe {
            sift_read_dir(
                dir.as_raw_fd(),
                scratch.as_mut_ptr().cast(),
                scratch.len() * 8,
                entries.as_mut_ptr().cast(),
                entries.len() as i32,
            )
        };
        if n < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if n == 0 {
            break;
        }
        ensure!(
            (n as usize) <= entries.len(),
            "native scanner returned an invalid count"
        );
        for item in &entries[..n as usize] {
            // C initialized these entries; name pointers belong to scratch until the next call.
            let e = unsafe { item.assume_init() };
            if e.error != 0 || e.name.is_null() || e.kind > 2 {
                skipped += 1;
                continue;
            }
            let bytes =
                unsafe { std::slice::from_raw_parts(e.name.cast::<u8>(), e.name_len as usize) };
            let Ok(name) = std::str::from_utf8(bytes) else {
                skipped += 1;
                continue;
            };
            if name == "." || name == ".." {
                continue;
            }
            visit(name, e, unsafe { lumi_dataless(e.flags) } != 0)?;
        }
    }
    Ok(skipped)
}

pub(crate) struct Pinyin {
    readings: Vec<String>,
    phrases: HashMap<&'static str, (&'static str, &'static str)>,
}
impl Pinyin {
    pub fn new() -> Result<Self> {
        let input = (0x4e00..=0x9fff)
            .map(|n| char::from_u32(n).unwrap().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let mut output = vec![0u8; 1 << 20];
        ensure!(
            unsafe {
                lumi_transliterate(
                    input.as_ptr(),
                    input.len(),
                    output.as_mut_ptr().cast(),
                    output.len(),
                )
            } != 0,
            "pinyin table generation failed"
        );
        let text = unsafe { CStr::from_ptr(output.as_ptr().cast()) }.to_str()?;
        let readings: Vec<String> = text
            .split('\n')
            .map(|s| {
                s.chars()
                    .filter(|c| c.is_ascii_alphabetic())
                    .flat_map(char::to_lowercase)
                    .collect()
            })
            .collect();
        ensure!(readings.len() == 0x5200, "unexpected pinyin table length");
        let phrases = include_str!("../vendor/pinyin-pro/phrases.tsv")
            .lines()
            .map(|line| {
                let mut fields = line.split('\t');
                (
                    fields.next().unwrap(),
                    (fields.next().unwrap(), fields.next().unwrap()),
                )
            })
            .collect();
        Ok(Self { readings, phrases })
    }
    pub fn keys(&self, name: &str) -> (String, String) {
        if !name.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
            return (String::new(), String::new());
        }
        let (mut full, mut initials) = (String::new(), String::new());
        let mut i = 0;
        while i < name.len() {
            let c = name[i..].chars().next().unwrap();
            if ('\u{4e00}'..='\u{9fff}').contains(&c) {
                // Dictionary phrases are 2–5 basic CJK characters (three UTF-8 bytes each).
                let phrase = (2..=5).rev().find_map(|n| {
                    name.get(i..i + n * 3)
                        .and_then(|word| self.phrases.get(word))
                        .map(|keys| (n * 3, keys))
                });
                if let Some((bytes, (pinyin, short))) = phrase {
                    full.push_str(pinyin);
                    initials.push_str(short);
                    i += bytes;
                    continue;
                }
                let reading = &self.readings[c as usize - 0x4e00];
                full.push_str(reading);
                if let Some(first) = reading.chars().next() {
                    initials.push(first);
                }
            } else {
                full.push(c);
                initials.push(c);
            }
            i += c.len_utf8();
        }
        (full, initials)
    }
}

#[derive(Debug)]
pub(crate) struct Event {
    pub path: PathBuf,
    pub flags: u32,
    pub id: u64,
}
pub(crate) struct Watcher {
    native: *mut c_void,
    _sender: Box<Sender<Event>>,
    pub events: Receiver<Event>,
}
extern "C" fn callback(ctx: *mut c_void, path: *const c_char, flags: u32, id: u64) {
    // Callback only copies data into the channel. It never mutates the index or unwinds through C.
    let sender = unsafe { &*ctx.cast::<Sender<Event>>() };
    let raw = unsafe { CStr::from_ptr(path) }.to_bytes();
    let raw = raw
        .strip_prefix(b"/System/Volumes/Data/")
        .map(|s| [b"/".as_slice(), s].concat())
        .unwrap_or_else(|| raw.to_vec());
    let _ = sender.send(Event {
        path: PathBuf::from(std::ffi::OsStr::from_bytes(&raw)),
        flags,
        id,
    });
}
impl Watcher {
    pub fn start(root: &Path, since: u64) -> Result<Self> {
        let root = CString::new(root.as_os_str().as_bytes())?;
        let (tx, events) = mpsc::channel();
        let mut sender = Box::new(tx);
        let native = unsafe {
            lumi_watch_start(
                root.as_ptr(),
                since,
                callback,
                (&mut *sender as *mut Sender<Event>).cast(),
            )
        };
        if native.is_null() {
            bail!("FSEvents could not start");
        }
        Ok(Self {
            native,
            _sender: sender,
            events,
        })
    }
}
impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe {
            lumi_watch_stop(self.native);
        }
    }
}
