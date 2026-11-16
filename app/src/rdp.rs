//! Embedded RDP sessions: the Microsoft Remote Desktop ActiveX control
//! (mstscax.dll, the one used by mstsc and RDM) hosted in a child window of
//! the Slint window through the system ATL host (`atl.dll`, `AtlAxWin`).

use zeroize::Zeroizing;

pub struct Params {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: String,
    pub password: Option<Zeroizing<String>>,
    /// Initial desktop size in physical pixels.
    pub width: u32,
    pub height: u32,
}

/// Connection state reported by the control (`Connected` property).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Disconnected,
    Connected,
    Connecting,
}

#[cfg(windows)]
pub use imp::RdpSession;

#[cfg(windows)]
mod imp {
    use super::{Params, State};
    use std::ffi::c_void;
    use windows::core::{s, w, IUnknown, Interface, BSTR, GUID, HSTRING, PCWSTR};
    use windows::Win32::Foundation::{HWND, VARIANT_FALSE, VARIANT_TRUE};
    use windows::Win32::System::Com::{IDispatch, DISPATCH_FLAGS, DISPATCH_METHOD, DISPATCH_PROPERTYGET, DISPATCH_PROPERTYPUT, DISPPARAMS};
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
    use windows::Win32::System::Variant::{VariantClear, VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_BSTR, VT_I4};
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, MoveWindow, ShowWindow, SW_HIDE, SW_SHOWNA, WINDOW_EX_STYLE, WS_CHILD, WS_CLIPCHILDREN,
        WS_CLIPSIBLINGS,
    };

    /// Newest first: MsRdpClient12…9 NotSafeForScripting.
    const CLSIDS: [GUID; 4] = [
        GUID::from_u128(0x3f859aa3_c2d4_4faa_b0e4_fd0c9c4e5e3a),
        GUID::from_u128(0x1df7c823_b2d4_4b54_975a_f2ac5d7cf8b8),
        GUID::from_u128(0xa0c63c30_f08d_4ab4_907c_34905d770c7d),
        GUID::from_u128(0x8b918b82_7985_4c24_89df_c33ad2bbfbcd),
    ];
    /// IMsRdpClientNonScriptable3 (vtable-only interface).
    const IID_NONSCRIPTABLE3: GUID = GUID::from_u128(0xb3378d90_0728_45c7_8ed7_b6159fb92219);
    const VT_PUT_ENABLE_CREDSSP: usize = 23;
    const IID_MSRDPCLIENT10: GUID = GUID::from_u128(0x7ed92c39_eb38_4927_a70a_708ac5a59321);
    const IID_MSRDPCLIENT9: GUID = GUID::from_u128(0x28904001_04b6_436c_a55b_0af1a0883dc9);
    const IID_MSTSCAX: GUID = GUID::from_u128(0x8c11efae_92c3_11d1_bc1e_00c04fa31489);
    const DISPID_PROPERTYPUT: i32 = -3;
    const LOCALE_USER_DEFAULT: u32 = 0x0400;

    type AtlAxWinInit = unsafe extern "system" fn() -> i32;
    type AtlAxAttachControl = unsafe extern "system" fn(*mut c_void, HWND, *mut *mut c_void) -> windows::core::HRESULT;

    pub struct RdpSession {
        hwnd: HWND,
        ax: IDispatch,
        size: (u32, u32),
        visible: bool,
    }

    fn variant(vt: windows::Win32::System::Variant::VARENUM, data: VARIANT_0_0_0) -> VARIANT {
        VARIANT {
            Anonymous: VARIANT_0 {
                Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                    vt,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: data,
                }),
            },
        }
    }
    fn v_str(s: &str) -> VARIANT {
        variant(
            VT_BSTR,
            VARIANT_0_0_0 {
                bstrVal: std::mem::ManuallyDrop::new(BSTR::from(s)),
            },
        )
    }
    fn v_i4(v: i32) -> VARIANT {
        variant(VT_I4, VARIANT_0_0_0 { lVal: v })
    }
    fn v_bool(v: bool) -> VARIANT {
        variant(
            VT_BOOL,
            VARIANT_0_0_0 {
                boolVal: if v { VARIANT_TRUE } else { VARIANT_FALSE },
            },
        )
    }

    fn dispid(d: &IDispatch, name: &str) -> windows::core::Result<i32> {
        let n = HSTRING::from(name);
        let names = [PCWSTR(n.as_ptr())];
        let mut id = 0;
        unsafe { d.GetIDsOfNames(&GUID::zeroed(), names.as_ptr(), 1, LOCALE_USER_DEFAULT, &mut id) }
            .map_err(|e| windows::core::Error::new(e.code(), format!("{name} : {}", e.message())))?;
        Ok(id)
    }

    fn invoke(d: &IDispatch, name: &str, flags: DISPATCH_FLAGS, mut args: Vec<VARIANT>) -> windows::core::Result<VARIANT> {
        let id = dispid(d, name)?;
        // Arguments are passed in reverse order.
        args.reverse();
        let mut named = DISPID_PROPERTYPUT;
        let params = DISPPARAMS {
            rgvarg: if args.is_empty() { std::ptr::null_mut() } else { args.as_mut_ptr() },
            rgdispidNamedArgs: if flags == DISPATCH_PROPERTYPUT {
                &mut named
            } else {
                std::ptr::null_mut()
            },
            cArgs: args.len() as u32,
            cNamedArgs: u32::from(flags == DISPATCH_PROPERTYPUT),
        };
        let mut result = VARIANT::default();
        let r = unsafe {
            d.Invoke(
                id,
                &GUID::zeroed(),
                LOCALE_USER_DEFAULT,
                flags,
                &params,
                Some(&mut result),
                None,
                None,
            )
        }
        .map_err(|e| windows::core::Error::new(e.code(), format!("{name} : {}", e.message())));
        for a in args.iter_mut() {
            unsafe {
                let _ = VariantClear(a);
            }
        }
        r.map(|_| result)
    }

    fn put(d: &IDispatch, name: &str, v: VARIANT) -> windows::core::Result<()> {
        invoke(d, name, DISPATCH_PROPERTYPUT, vec![v]).map(|mut r| unsafe {
            let _ = VariantClear(&mut r);
        })
    }

    fn get_dispatch(d: &IDispatch, name: &str) -> windows::core::Result<IDispatch> {
        let r = invoke(d, name, DISPATCH_PROPERTYGET, vec![])?;
        let disp = unsafe { (*r.Anonymous.Anonymous).Anonymous.pdispVal.clone() };
        std::mem::forget(r);
        std::mem::ManuallyDrop::into_inner(disp).ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_NOINTERFACE))
    }

    fn get_i32(d: &IDispatch, name: &str) -> windows::core::Result<i32> {
        let mut r = invoke(d, name, DISPATCH_PROPERTYGET, vec![])?;
        let v = unsafe {
            let inner = &*r.Anonymous.Anonymous;
            match inner.vt.0 {
                2 => inner.Anonymous.iVal as i32,
                _ => inner.Anonymous.lVal,
            }
        };
        unsafe {
            let _ = VariantClear(&mut r);
        }
        Ok(v)
    }

    /// Calls a vtable-only `put_X(VARIANT_BOOL)` through QueryInterface.
    fn put_vtable_bool(unk: &IUnknown, iid: &GUID, slot: usize, value: bool) -> Result<(), String> {
        let mut ptr: *mut c_void = std::ptr::null_mut();
        unsafe {
            unk.query(iid, &mut ptr).ok().map_err(|e| e.to_string())?;
            let vtbl = *(ptr as *const *const usize);
            let f: unsafe extern "system" fn(*mut c_void, i16) -> windows::core::HRESULT = std::mem::transmute(*vtbl.add(slot));
            let hr = f(ptr, if value { -1 } else { 0 });
            let release: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*vtbl.add(2));
            release(ptr);
            hr.ok().map_err(|e| e.to_string())
        }
    }

    impl RdpSession {
        /// Creates the control inside `parent` and starts connecting.
        pub fn connect(parent: isize, rect: (i32, i32, i32, i32), p: &Params) -> Result<RdpSession, String> {
            unsafe {
                let atl = LoadLibraryW(w!("atl.dll")).map_err(|e| format!("atl.dll : {e}"))?;
                let init: AtlAxWinInit = std::mem::transmute(GetProcAddress(atl, s!("AtlAxWinInit")).ok_or("AtlAxWinInit absent")?);
                let attach: AtlAxAttachControl =
                    std::mem::transmute(GetProcAddress(atl, s!("AtlAxAttachControl")).ok_or("AtlAxAttachControl absent")?);
                if init() == 0 {
                    return Err("initialisation de l'hôte ActiveX impossible".into());
                }
                let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
                let (x, y, cw, ch) = rect;
                // STA + OLE on the UI thread; harmless if already initialized.
                let _ = windows::Win32::System::Ole::OleInitialize(None);
                let mut created = None;
                let mut last_err = String::from("contrôle Bureau à distance (mstscax.dll) indisponible");
                for clsid in CLSIDS {
                    let unk: IUnknown = match windows::Win32::System::Com::CoCreateInstance(
                        &clsid,
                        None,
                        windows::Win32::System::Com::CLSCTX_INPROC_SERVER,
                    ) {
                        Ok(u) => u,
                        Err(e) => {
                            last_err = format!("création du contrôle RDP : {e}");
                            continue;
                        }
                    };
                    // The control's default IDispatch is not the RDP one: ask for the dual interfaces.
                    let Some(ax) = [IID_MSRDPCLIENT10, IID_MSRDPCLIENT9, IID_MSTSCAX].iter().find_map(|iid| {
                        let mut p: *mut c_void = std::ptr::null_mut();
                        (unk.query(iid, &mut p).is_ok() && !p.is_null()).then(|| IDispatch::from_raw(p))
                    }) else {
                        continue;
                    };
                    let Ok(hwnd) = CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        w!("AtlAxWin"),
                        w!(""),
                        WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                        x,
                        y,
                        cw.max(1),
                        ch.max(1),
                        Some(HWND(parent as *mut c_void)),
                        None,
                        Some(instance.into()),
                        None,
                    ) else {
                        continue;
                    };
                    if let Err(e) = attach(unk.as_raw(), hwnd, std::ptr::null_mut()).ok() {
                        last_err = format!("hébergement du contrôle RDP : {e}");
                        let _ = DestroyWindow(hwnd);
                        continue;
                    }
                    created = Some((hwnd, unk, ax));
                    break;
                }
                let (hwnd, unk, ax) = created.ok_or(last_err)?;
                let session = RdpSession {
                    hwnd,
                    ax,
                    size: (p.width, p.height),
                    visible: false,
                };
                session.configure(&unk, p).map_err(|e| format!("configuration RDP : {e}"))?;
                invoke(&session.ax, "Connect", DISPATCH_METHOD, vec![]).map_err(|e| format!("connexion RDP : {e}"))?;
                Ok(session)
            }
        }

        fn configure(&self, unk: &IUnknown, p: &Params) -> windows::core::Result<()> {
            let ax = &self.ax;
            put(ax, "Server", v_str(&p.host))?;
            if !p.username.is_empty() {
                put(ax, "UserName", v_str(&p.username))?;
            }
            if !p.domain.is_empty() {
                put(ax, "Domain", v_str(&p.domain))?;
            }
            put(ax, "DesktopWidth", v_i4(p.width.clamp(200, 8192) as i32))?;
            put(ax, "DesktopHeight", v_i4(p.height.clamp(200, 8192) as i32))?;
            let _ = put(ax, "ColorDepth", v_i4(32));
            let _ = put(ax, "ConnectingText", v_str("Connexion en cours…"));
            let _ = put(ax, "DisconnectedText", v_str("Session déconnectée"));
            let adv = get_dispatch(ax, "AdvancedSettings9").or_else(|_| get_dispatch(ax, "AdvancedSettings2"))?;
            put(&adv, "RDPPort", v_i4(p.port as i32))?;
            if let Some(pw) = p.password.as_ref().filter(|s| !s.is_empty()) {
                put(&adv, "ClearTextPassword", v_str(pw))?;
            }
            // Server identity checks stay on: the control warns about unknown certificates.
            let _ = put(&adv, "AuthenticationLevel", v_i4(2));
            let _ = put(&adv, "EnableCredSspSupport", v_bool(true));
            let _ = put(&adv, "RedirectClipboard", v_bool(true));
            let _ = put(&adv, "EnableAutoReconnect", v_bool(true));
            let _ = put(&adv, "SmartSizing", v_bool(true));
            let _ = put(&adv, "GrabFocusOnConnect", v_bool(true));
            // NLA (CredSSP) is only reachable through the non-scriptable interface.
            let _ = put_vtable_bool(unk, &IID_NONSCRIPTABLE3, VT_PUT_ENABLE_CREDSSP, true);
            Ok(())
        }

        pub fn state(&self) -> State {
            match get_i32(&self.ax, "Connected") {
                Ok(1) => State::Connected,
                Ok(2) => State::Connecting,
                _ => State::Disconnected,
            }
        }

        /// Places the control over the tab area (client pixels) and shows/hides it.
        pub fn place(&mut self, rect: (i32, i32, i32, i32), visible: bool) {
            unsafe {
                if visible {
                    let _ = MoveWindow(self.hwnd, rect.0, rect.1, rect.2.max(1), rect.3.max(1), true);
                }
                if visible != self.visible {
                    let _ = ShowWindow(self.hwnd, if visible { SW_SHOWNA } else { SW_HIDE });
                    if visible {
                        let _ = SetFocus(Some(self.hwnd));
                    }
                    self.visible = visible;
                }
            }
        }

        /// Asks the server to adopt a new resolution (dynamic resolution, RDP 8.1+).
        pub fn resize_desktop(&mut self, width: u32, height: u32) {
            let (width, height) = (width.clamp(200, 8192) & !1, height.clamp(200, 8192));
            if (width, height) == self.size || self.state() != State::Connected {
                return;
            }
            let args = vec![
                v_i4(width as i32),
                v_i4(height as i32),
                v_i4(0),
                v_i4(0),
                v_i4(0),
                v_i4(100),
                v_i4(100),
            ];
            if invoke(&self.ax, "UpdateSessionDisplaySettings", DISPATCH_METHOD, args).is_ok() {
                self.size = (width, height);
            }
        }

        pub fn disconnect(&self) {
            if self.state() != State::Disconnected {
                let _ = invoke(&self.ax, "Disconnect", DISPATCH_METHOD, vec![]);
            }
        }
    }

    impl Drop for RdpSession {
        fn drop(&mut self) {
            self.disconnect();
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

#[cfg(not(windows))]
pub struct RdpSession;

#[cfg(not(windows))]
impl RdpSession {
    pub fn connect(_: isize, _: (i32, i32, i32, i32), _: &Params) -> Result<RdpSession, String> {
        Err("disponible uniquement sous Windows".into())
    }
    pub fn state(&self) -> State {
        State::Disconnected
    }
    pub fn place(&mut self, _: (i32, i32, i32, i32), _: bool) {}
    pub fn resize_desktop(&mut self, _: u32, _: u32) {}
    pub fn disconnect(&self) {}
}
