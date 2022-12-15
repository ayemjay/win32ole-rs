use crate::{error::Result, ToWide, G_RUNNING_NANO};
use std::{ffi::OsStr, ptr};
use windows::{
    core::{Interface, BSTR, GUID, PCWSTR},
    Win32::System::{
        Com::{
            CLSIDFromProgID, CLSIDFromString, CoCreateInstance, CoInitializeEx, CoUninitialize,
            ITypeInfo, ITypeLib, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED,
            TYPEDESC, VT_PTR, VT_SAFEARRAY,
        },
        Ole::{OleInitialize, OleUninitialize},
    },
};

thread_local!(static OLE_INITIALIZED: OleInitialized = {
    unsafe {
        let result = if *G_RUNNING_NANO {
            CoInitializeEx(None, COINIT_MULTITHREADED)
        } else {
            OleInitialize(ptr::null_mut())
        };
        if let Err(error) = result {
            panic!("Failed: OLE initialization. {error}");
        }
        OleInitialized(ptr::null_mut())
    }
});

/// RAII object that guards the fact that COM is initialized.
///
// We store a raw pointer because it's the only way at the moment to remove `Send`/`Sync` from the
// object.
struct OleInitialized(*mut ());

impl Drop for OleInitialized {
    #[inline]
    fn drop(&mut self) {
        if *G_RUNNING_NANO {
            unsafe { CoUninitialize() };
        } else {
            unsafe { OleUninitialize() };
        }
    }
}

/// Ensures that COM is initialized in this thread.
#[inline]
pub fn ole_initialized() {
    OLE_INITIALIZED.with(|_| {});
}

pub fn get_class_id<S: AsRef<OsStr>>(s: S) -> Result<GUID> {
    let prog_id = s.to_wide_null();
    let prog_id = PCWSTR::from_raw(prog_id.as_ptr());

    unsafe {
        match CLSIDFromProgID(prog_id) {
            Ok(guid) => Ok(guid),
            Err(_error) => match CLSIDFromString(prog_id) {
                Ok(guid) => Ok(guid),
                Err(error) => Err(error.into()),
            },
        }
    }
}

pub fn create_instance<T: Interface>(clsid: &GUID) -> Result<T> {
    let flags = CLSCTX_INPROC_SERVER | CLSCTX_LOCAL_SERVER;
    unsafe { Ok(CoCreateInstance(clsid, None, flags)?) }
}

pub fn create_com_object<S: AsRef<OsStr>, T: Interface>(s: S) -> Result<T> {
    ole_initialized();
    let class_id = get_class_id(s)?;

    create_instance(&class_id)
}

pub(crate) fn ole_typedesc2val(
    typeinfo: &ITypeInfo,
    typedesc: &TYPEDESC,
    mut typedetails: Option<&mut Vec<String>>,
) -> String {
    let typestr = match typedesc.vt.0 {
        2 => "I2".into(),
        3 => "I4".into(),
        4 => "R4".into(),
        5 => "R8".into(),
        6 => "CY".into(),
        7 => "DATE".into(),
        8 => "BSTR".into(),
        11 => "BOOL".into(),
        12 => "VARIANT".into(),
        14 => "DECIMAL".into(),
        16 => "I1".into(),
        17 => "UI1".into(),
        18 => "UI2".into(),
        19 => "UI4".into(),
        20 => "I8".into(),
        21 => "UI8".into(),
        22 => "INT".into(),
        23 => "UINT".into(),
        24 => "VOID".into(),
        25 => "HRESULT".into(),
        26 => {
            let typestr: String = "PTR".into();
            if let Some(ref mut typedetails) = typedetails {
                typedetails.push(typestr);
            }
            return ole_ptrtype2val(typeinfo, typedesc, typedetails);
        }
        27 => {
            let typestr: String = "SAFEARRAY".into();
            if let Some(ref mut typedetails) = typedetails {
                typedetails.push(typestr);
            }
            return ole_ptrtype2val(typeinfo, typedesc, typedetails);
        }
        28 => "CARRAY".into(),
        29 => {
            let typestr: String = "USERDEFINED".into();
            if let Some(ref mut typedetails) = typedetails {
                typedetails.push(typestr.clone());
            }
            let str = ole_usertype2val(typeinfo, typedesc, typedetails);
            if let Some(str) = str {
                return str;
            }
            return typestr;
        }
        13 => "UNKNOWN".into(),
        9 => "DISPATCH".into(),
        10 => "ERROR".into(),
        31 => "LPWSTR".into(),
        30 => "LPSTR".into(),
        36 => "RECORD".into(),
        _ => {
            let typestr: String = "Unknown Type ".into();
            format!("{}{}", typestr, typedesc.vt.0)
        }
    };
    if let Some(typedetails) = typedetails {
        typedetails.push(typestr.clone());
    }
    typestr
}

pub(crate) fn ole_ptrtype2val(
    typeinfo: &ITypeInfo,
    typedesc: &TYPEDESC,
    typedetails: Option<&mut Vec<String>>,
) -> String {
    let mut type_ = "".into();

    if typedesc.vt == VT_PTR || typedesc.vt == VT_SAFEARRAY {
        let p = unsafe { typedesc.Anonymous.lptdesc };
        type_ = ole_typedesc2val(typeinfo, unsafe { &*p }, typedetails);
    }
    type_
}

pub(crate) fn ole_usertype2val(
    typeinfo: &ITypeInfo,
    typedesc: &TYPEDESC,
    typedetails: Option<&mut Vec<String>>,
) -> Option<String> {
    let result = unsafe { typeinfo.GetRefTypeInfo(typedesc.Anonymous.hreftype) };
    if result.is_err() {
        return None;
    }
    let reftypeinfo = result.unwrap();
    let mut bstrname = BSTR::default();
    let result = ole_docinfo_from_type(
        &reftypeinfo,
        Some(&mut bstrname),
        None,
        ptr::null_mut(),
        None,
    );
    if result.is_err() {
        return None;
    }
    let type_ = bstrname.to_string();
    if let Some(typedetails) = typedetails {
        typedetails.push(type_.clone());
    }
    Some(type_)
}

pub(crate) fn ole_docinfo_from_type(
    typeinfo: &ITypeInfo,
    name: Option<*mut BSTR>,
    helpstr: Option<*mut BSTR>,
    helpcontext: *mut u32,
    helpfile: Option<*mut BSTR>,
) -> Result<()> {
    let mut typelib: Option<ITypeLib> = None;
    let mut index = 0;
    unsafe { typeinfo.GetContainingTypeLib(&mut typelib, &mut index)? };
    let typelib = typelib.unwrap();
    unsafe { typelib.GetDocumentation(index as i32, name, helpstr, helpcontext, helpfile)? };
    Ok(())
}
