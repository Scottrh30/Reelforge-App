//! Property sets (the OFX way of passing everything around) and the
//! property suite plugins use to read and write them.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};

pub type Status = c_int;
pub const OK: Status = 0;
pub const FAILED: Status = 1;
pub const ERR_UNKNOWN: Status = 3;
#[allow(dead_code)]
pub const ERR_UNSUPPORTED: Status = 5;
pub const ERR_MEMORY: Status = 8;
pub const ERR_BAD_HANDLE: Status = 9;
pub const ERR_BAD_INDEX: Status = 10;
pub const ERR_VALUE: Status = 11;
pub const REPLY_DEFAULT: Status = 14;

#[derive(Clone, Debug)]
pub enum Val {
    Ptr(Vec<*mut c_void>),
    Str(Vec<CString>),
    Dbl(Vec<f64>),
    Int(Vec<c_int>),
}

/// A property set. Plugins hold pointers to these (as handles), so each lives in a `Box`.
#[derive(Clone, Debug, Default)]
pub struct PropSet {
    pub map: HashMap<String, Val>,
}

fn cstring(s: &str) -> CString {
    // (no NULs left in it, so it always converts)
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

impl PropSet {
    pub fn new() -> Box<Self> {
        Box::default()
    }
    pub fn s(&mut self, k: &str, v: &str) -> &mut Self {
        self.map.insert(k.into(), Val::Str(vec![cstring(v)]));
        self
    }
    pub fn sn(&mut self, k: &str, v: &[&str]) -> &mut Self {
        self.map.insert(k.into(), Val::Str(v.iter().map(|s| cstring(s)).collect()));
        self
    }
    pub fn i(&mut self, k: &str, v: c_int) -> &mut Self {
        self.map.insert(k.into(), Val::Int(vec![v]));
        self
    }
    pub fn in_(&mut self, k: &str, v: &[c_int]) -> &mut Self {
        self.map.insert(k.into(), Val::Int(v.to_vec()));
        self
    }
    pub fn d(&mut self, k: &str, v: f64) -> &mut Self {
        self.map.insert(k.into(), Val::Dbl(vec![v]));
        self
    }
    pub fn dn(&mut self, k: &str, v: &[f64]) -> &mut Self {
        self.map.insert(k.into(), Val::Dbl(v.to_vec()));
        self
    }
    pub fn p(&mut self, k: &str, v: *mut c_void) -> &mut Self {
        self.map.insert(k.into(), Val::Ptr(vec![v]));
        self
    }

    pub fn get_str(&self, k: &str, i: usize) -> Option<String> {
        match self.map.get(k)? {
            Val::Str(v) => v.get(i).map(|s| s.to_string_lossy().into_owned()),
            _ => None,
        }
    }
    pub fn strs(&self, k: &str) -> Vec<String> {
        match self.map.get(k) {
            Some(Val::Str(v)) => v.iter().map(|s| s.to_string_lossy().into_owned()).collect(),
            _ => Vec::new(),
        }
    }
    pub fn get_dbls(&self, k: &str) -> Vec<f64> {
        match self.map.get(k) {
            Some(Val::Dbl(v)) => v.clone(),
            Some(Val::Int(v)) => v.iter().map(|&x| x as f64).collect(),
            _ => Vec::new(),
        }
    }
    pub fn get_int(&self, k: &str, i: usize) -> Option<c_int> {
        match self.map.get(k)? {
            Val::Int(v) => v.get(i).copied(),
            Val::Dbl(v) => v.get(i).map(|&x| x.round() as c_int),
            _ => None,
        }
    }
    #[allow(dead_code)]
    pub fn get_ptr(&self, k: &str) -> *mut c_void {
        match self.map.get(k) {
            Some(Val::Ptr(v)) => v.first().copied().unwrap_or(std::ptr::null_mut()),
            _ => std::ptr::null_mut(),
        }
    }
}

/// The set behind a handle (null: none).
///
/// # Safety
/// `h` is null or a pointer to a live `PropSet` the host made.
unsafe fn set<'a>(h: *mut c_void) -> Option<&'a mut PropSet> {
    (h as *mut PropSet).as_mut()
}

unsafe fn name<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        None
    } else {
        CStr::from_ptr(p).to_str().ok()
    }
}

/// Run `f`, turning a panic into a failure (unwinding into C would abort).
pub fn guard(f: impl FnOnce() -> Status) -> Status {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(FAILED)
}

/// Put `value` at `index` of property `prop`, making or converting it as needed.
fn put(ps: &mut PropSet, prop: &str, index: usize, value: Val) -> Status {
    if index > 1024 {
        return ERR_BAD_INDEX;
    }
    let entry = ps.map.entry(prop.to_string()).or_insert_with(|| match &value {
        Val::Ptr(_) => Val::Ptr(Vec::new()),
        Val::Str(_) => Val::Str(Vec::new()),
        Val::Dbl(_) => Val::Dbl(Vec::new()),
        Val::Int(_) => Val::Int(Vec::new()),
    });
    match (entry, value) {
        (Val::Ptr(v), Val::Ptr(x)) => set_at(v, index, x[0], std::ptr::null_mut()),
        (Val::Str(v), Val::Str(x)) => set_at(v, index, x[0].clone(), CString::default()),
        (Val::Dbl(v), Val::Dbl(x)) => set_at(v, index, x[0], 0.0),
        (Val::Int(v), Val::Int(x)) => set_at(v, index, x[0], 0),
        // A number of the other kind: converted.
        (Val::Dbl(v), Val::Int(x)) => set_at(v, index, x[0] as f64, 0.0),
        (Val::Int(v), Val::Dbl(x)) => set_at(v, index, x[0].round() as c_int, 0),
        (e, x) => {
            // Something else: the property becomes this kind.
            *e = x;
            if index > 0 {
                return ERR_VALUE;
            }
        }
    }
    OK
}

fn set_at<T: Clone>(v: &mut Vec<T>, index: usize, x: T, fill: T) {
    if v.len() <= index {
        v.resize(index + 1, fill);
    }
    v[index] = x;
}

macro_rules! setter {
    ($fname:ident, $t:ty, $wrap:expr) => {
        pub unsafe extern "C" fn $fname(h: *mut c_void, prop: *const c_char, index: c_int, value: $t) -> Status {
            guard(|| {
                let (Some(ps), Some(prop)) = (set(h), name(prop)) else { return ERR_BAD_HANDLE };
                if index < 0 {
                    return ERR_BAD_INDEX;
                }
                put(ps, prop, index as usize, $wrap(value))
            })
        }
    };
}
setter!(prop_set_pointer, *mut c_void, |v| Val::Ptr(vec![v]));
setter!(prop_set_double, f64, |v| Val::Dbl(vec![v]));
setter!(prop_set_int, c_int, |v| Val::Int(vec![v]));
pub unsafe extern "C" fn prop_set_string(h: *mut c_void, prop: *const c_char, index: c_int, value: *const c_char) -> Status {
    guard(|| {
        let (Some(ps), Some(prop)) = (set(h), name(prop)) else { return ERR_BAD_HANDLE };
        if index < 0 {
            return ERR_BAD_INDEX;
        }
        let v = if value.is_null() { CString::default() } else { CStr::from_ptr(value).to_owned() };
        put(ps, prop, index as usize, Val::Str(vec![v]))
    })
}

macro_rules! setter_n {
    ($fname:ident, $t:ty, $one:ident) => {
        pub unsafe extern "C" fn $fname(h: *mut c_void, prop: *const c_char, count: c_int, values: *const $t) -> Status {
            if values.is_null() || count < 0 {
                return ERR_VALUE;
            }
            for i in 0..count {
                let s = $one(h, prop, i, *values.add(i as usize));
                if s != OK {
                    return s;
                }
            }
            OK
        }
    };
}
setter_n!(prop_set_pointer_n, *mut c_void, prop_set_pointer);
setter_n!(prop_set_double_n, f64, prop_set_double);
setter_n!(prop_set_int_n, c_int, prop_set_int);
setter_n!(prop_set_string_n, *const c_char, prop_set_string);

pub unsafe extern "C" fn prop_get_pointer(h: *mut c_void, prop: *const c_char, index: c_int, out: *mut *mut c_void) -> Status {
    guard(|| {
        let (Some(ps), Some(prop)) = (set(h), name(prop)) else { return ERR_BAD_HANDLE };
        match ps.map.get(prop) {
            None => ERR_UNKNOWN,
            Some(Val::Ptr(v)) => match v.get(index as usize) {
                Some(&p) if index >= 0 => {
                    *out = p;
                    OK
                }
                _ => ERR_BAD_INDEX,
            },
            Some(_) => ERR_VALUE,
        }
    })
}

pub unsafe extern "C" fn prop_get_string(h: *mut c_void, prop: *const c_char, index: c_int, out: *mut *mut c_char) -> Status {
    guard(|| {
        let (Some(ps), Some(prop)) = (set(h), name(prop)) else { return ERR_BAD_HANDLE };
        match ps.map.get(prop) {
            None => ERR_UNKNOWN,
            Some(Val::Str(v)) => match v.get(index as usize) {
                // The string stays where it is until the property changes.
                Some(s) if index >= 0 => {
                    *out = s.as_ptr() as *mut c_char;
                    OK
                }
                _ => ERR_BAD_INDEX,
            },
            Some(_) => ERR_VALUE,
        }
    })
}

pub unsafe extern "C" fn prop_get_double(h: *mut c_void, prop: *const c_char, index: c_int, out: *mut f64) -> Status {
    guard(|| {
        let (Some(ps), Some(prop)) = (set(h), name(prop)) else { return ERR_BAD_HANDLE };
        let v = match ps.map.get(prop) {
            None => return ERR_UNKNOWN,
            Some(Val::Dbl(v)) => v.get(index as usize).copied(),
            Some(Val::Int(v)) => v.get(index as usize).map(|&x| x as f64),
            Some(_) => return ERR_VALUE,
        };
        match v {
            Some(x) if index >= 0 => {
                *out = x;
                OK
            }
            _ => ERR_BAD_INDEX,
        }
    })
}

pub unsafe extern "C" fn prop_get_int(h: *mut c_void, prop: *const c_char, index: c_int, out: *mut c_int) -> Status {
    guard(|| {
        let (Some(ps), Some(prop)) = (set(h), name(prop)) else { return ERR_BAD_HANDLE };
        let v = match ps.map.get(prop) {
            None => return ERR_UNKNOWN,
            Some(Val::Int(v)) => v.get(index as usize).copied(),
            Some(Val::Dbl(v)) => v.get(index as usize).map(|&x| x.round() as c_int),
            Some(_) => return ERR_VALUE,
        };
        match v {
            Some(x) if index >= 0 => {
                *out = x;
                OK
            }
            _ => ERR_BAD_INDEX,
        }
    })
}

macro_rules! getter_n {
    ($fname:ident, $t:ty, $one:ident) => {
        pub unsafe extern "C" fn $fname(h: *mut c_void, prop: *const c_char, count: c_int, out: *mut $t) -> Status {
            if out.is_null() || count < 0 {
                return ERR_VALUE;
            }
            for i in 0..count {
                let s = $one(h, prop, i, out.add(i as usize));
                if s != OK {
                    return s;
                }
            }
            OK
        }
    };
}
getter_n!(prop_get_pointer_n, *mut c_void, prop_get_pointer);
getter_n!(prop_get_string_n, *mut c_char, prop_get_string);
getter_n!(prop_get_double_n, f64, prop_get_double);
getter_n!(prop_get_int_n, c_int, prop_get_int);

pub unsafe extern "C" fn prop_reset(h: *mut c_void, prop: *const c_char) -> Status {
    guard(|| {
        let (Some(_), Some(_)) = (set(h), name(prop)) else { return ERR_BAD_HANDLE };
        OK
    })
}

pub unsafe extern "C" fn prop_get_dimension(h: *mut c_void, prop: *const c_char, count: *mut c_int) -> Status {
    guard(|| {
        let (Some(ps), Some(prop)) = (set(h), name(prop)) else { return ERR_BAD_HANDLE };
        let n = match ps.map.get(prop) {
            None => return ERR_UNKNOWN,
            Some(Val::Ptr(v)) => v.len(),
            Some(Val::Str(v)) => v.len(),
            Some(Val::Dbl(v)) => v.len(),
            Some(Val::Int(v)) => v.len(),
        };
        *count = n as c_int;
        OK
    })
}

type Set<T> = unsafe extern "C" fn(*mut c_void, *const c_char, c_int, T) -> Status;
type Get<T> = unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut T) -> Status;

#[repr(C)]
pub struct PropertySuiteV1 {
    pub set_pointer: Set<*mut c_void>,
    pub set_string: Set<*const c_char>,
    pub set_double: Set<f64>,
    pub set_int: Set<c_int>,
    pub set_pointer_n: Set<*const *mut c_void>,
    pub set_string_n: Set<*const *const c_char>,
    pub set_double_n: Set<*const f64>,
    pub set_int_n: Set<*const c_int>,
    pub get_pointer: Get<*mut c_void>,
    pub get_string: Get<*mut c_char>,
    pub get_double: Get<f64>,
    pub get_int: Get<c_int>,
    pub get_pointer_n: Get<*mut c_void>,
    pub get_string_n: Get<*mut c_char>,
    pub get_double_n: Get<f64>,
    pub get_int_n: Get<c_int>,
    pub reset: unsafe extern "C" fn(*mut c_void, *const c_char) -> Status,
    pub get_dimension: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_int) -> Status,
}

pub static PROPERTY_SUITE: PropertySuiteV1 = PropertySuiteV1 {
    set_pointer: prop_set_pointer,
    set_string: prop_set_string,
    set_double: prop_set_double,
    set_int: prop_set_int,
    set_pointer_n: prop_set_pointer_n,
    set_string_n: prop_set_string_n,
    set_double_n: prop_set_double_n,
    set_int_n: prop_set_int_n,
    get_pointer: prop_get_pointer,
    get_string: prop_get_string,
    get_double: prop_get_double,
    get_int: prop_get_int,
    get_pointer_n: prop_get_pointer_n,
    get_string_n: prop_get_string_n,
    get_double_n: prop_get_double_n,
    get_int_n: prop_get_int_n,
    reset: prop_reset,
    get_dimension: prop_get_dimension,
};
