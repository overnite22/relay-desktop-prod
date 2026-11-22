//! Embedded RDP sessions: the Microsoft Remote Desktop ActiveX control
//! (mstscax.dll, the one used by mstsc and RDM) hosted in a child window of
//! the Slint window through the system ATL host (`atl.dll`, `AtlAxWin`).

use relay_desktop_core::conn::{RdpOptions, Resolution};
use zeroize::Zeroizing;

pub struct Params {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: String,
    pub password: Option<Zeroizing<String>>,
    /// Size of the tab in physical pixels.
    pub width: u32,
    pub height: u32,
    /// Settings of the entry.
    pub opts: RdpOptions,
    /// Resolved desktop size mode (never `Default`).
    pub resolution: Resolution,
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
    use relay_desktop_core::conn::{Gateway, Resolution};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::Arc;
    use windows::core::{s, w, IUnknown, Interface, BSTR, GUID, HSTRING, PCWSTR};
    use windows::Win32::Foundation::{HWND, VARIANT_FALSE, VARIANT_TRUE};
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows::Win32::System::Com::{
        IConnectionPoint, IConnectionPointContainer, IDispatch, IDispatch_Impl, IDispatch_Vtbl, ITypeInfo, DISPATCH_FLAGS, DISPATCH_METHOD,
        DISPATCH_PROPERTYGET, DISPATCH_PROPERTYPUT, DISPPARAMS, EXCEPINFO, INVOKE_PROPERTYPUT,
    };
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
    use windows::Win32::System::Ole::LoadTypeLib;
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
    /// `OnDisconnected` and `OnFatalError` of IMsTscAxEvents.
    const DISPID_ON_DISCONNECTED: i32 = 4;
    const DISPID_ON_FATAL_ERROR: i32 = 10;
    /// TS_PERF_DISABLE_WALLPAPER, TS_PERF_ENABLE_FONT_SMOOTHING, TS_PERF_ENABLE_DESKTOP_COMPOSITION.
    const PERF_NO_WALLPAPER: i32 = 0x1;
    const PERF_FONT_SMOOTHING: i32 = 0x80;
    const PERF_COMPOSITION: i32 = 0x100;

    type AtlAxWinInit = unsafe extern "system" fn() -> i32;
    type AtlAxAttachControl = unsafe extern "system" fn(*mut c_void, HWND, *mut *mut c_void) -> windows::core::HRESULT;

    /// Event interface of the control (dispinterface IMsTscAxEvents).
    #[windows::core::interface("336d5562-efa8-482e-8cb3-c5c0fc7a7db6")]
    unsafe trait IMsTscAxEvents: IDispatch {}

    /// Event sink: only keeps the disconnection / fatal error codes.
    #[windows::core::implement(IMsTscAxEvents)]
    struct Events {
        disc: Arc<AtomicI32>,
        fatal: Arc<AtomicI32>,
    }

    impl IMsTscAxEvents_Impl for Events_Impl {}

    impl IDispatch_Impl for Events_Impl {
        fn GetTypeInfoCount(&self) -> windows::core::Result<u32> {
            Ok(0)
        }
        fn GetTypeInfo(&self, _: u32, _: u32) -> windows::core::Result<ITypeInfo> {
            Err(windows::Win32::Foundation::E_NOTIMPL.into())
        }
        fn GetIDsOfNames(&self, _: *const GUID, _: *const PCWSTR, _: u32, _: u32, _: *mut i32) -> windows::core::Result<()> {
            Err(windows::Win32::Foundation::E_NOTIMPL.into())
        }
        fn Invoke(
            &self,
            id: i32,
            _: *const GUID,
            _: u32,
            _: DISPATCH_FLAGS,
            params: *const DISPPARAMS,
            _: *mut VARIANT,
            _: *mut EXCEPINFO,
            _: *mut u32,
        ) -> windows::core::Result<()> {
            let target = match id {
                DISPID_ON_DISCONNECTED => &self.disc,
                DISPID_ON_FATAL_ERROR => &self.fatal,
                _ => return Ok(()),
            };
            // Single LONG argument.
            unsafe {
                if let Some(p) = params.as_ref().filter(|p| p.cArgs >= 1 && !p.rgvarg.is_null()) {
                    let v = &*(*p.rgvarg).Anonymous.Anonymous;
                    target.store(
                        if v.vt == VT_I4 { v.Anonymous.lVal } else { v.Anonymous.iVal as i32 },
                        Ordering::Release,
                    );
                }
            }
            Ok(())
        }
    }

    pub struct RdpSession {
        hwnd: HWND,
        ax: IDispatch,
        size: (u32, u32),
        visible: bool,
        /// Follow the tab size (dynamic resolution).
        fit: bool,
        disc: Arc<AtomicI32>,
        fatal: Arc<AtomicI32>,
        events: Option<(IConnectionPoint, u32)>,
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

    fn get_bool(d: &IDispatch, name: &str) -> windows::core::Result<bool> {
        let mut r = invoke(d, name, DISPATCH_PROPERTYGET, vec![])?;
        let v = unsafe {
            let inner = &*r.Anonymous.Anonymous;
            match inner.vt {
                VT_BOOL => inner.Anonymous.boolVal.0 != 0,
                _ => inner.Anonymous.lVal != 0,
            }
        };
        unsafe {
            let _ = VariantClear(&mut r);
        }
        Ok(v)
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

    /// Calls `put_<prop>(VARIANT_BOOL)` of a vtable-only IMsRdpClientNonScriptable*
    /// interface, found by name in the control's type library (no hard-coded
    /// IID or vtable slot).
    fn put_nonscriptable_bool(unk: &IUnknown, prop: &str, value: bool) -> Result<(), String> {
        unsafe {
            let lib = LoadTypeLib(w!("mstscax.dll")).map_err(|e| e.to_string())?;
            for i in 0..lib.GetTypeInfoCount() {
                let mut name = BSTR::new();
                if lib
                    .GetDocumentation(i as i32, Some(&mut name), None, std::ptr::null_mut(), None)
                    .is_err()
                    || !name.to_string().starts_with("IMsRdpClientNonScriptable")
                {
                    continue;
                }
                let Ok(info) = lib.GetTypeInfo(i) else { continue };
                let Ok(attr) = info.GetTypeAttr() else { continue };
                let (iid, count) = ((*attr).guid, (*attr).cFuncs);
                info.ReleaseTypeAttr(attr);
                for f in 0..count {
                    let Ok(fd) = info.GetFuncDesc(f as u32) else { continue };
                    let (memid, kind, offset) = ((*fd).memid, (*fd).invkind, (*fd).oVft);
                    info.ReleaseFuncDesc(fd);
                    if kind != INVOKE_PROPERTYPUT {
                        continue;
                    }
                    let mut fname = BSTR::new();
                    if info
                        .GetDocumentation(memid, Some(&mut fname), None, std::ptr::null_mut(), None)
                        .is_ok()
                        && fname == prop
                    {
                        return put_vtable_bool(unk, &iid, offset as usize / std::mem::size_of::<usize>(), value);
                    }
                }
            }
        }
        Err(format!("{prop} : propriété absente de mstscax.dll"))
    }

    fn get_string(d: &IDispatch, name: &str, args: Vec<VARIANT>) -> Option<String> {
        let mut r = invoke(d, name, DISPATCH_METHOD, args).ok()?;
        let s = unsafe {
            let inner = &*r.Anonymous.Anonymous;
            (inner.vt == VT_BSTR).then(|| inner.Anonymous.bstrVal.to_string())
        };
        unsafe {
            let _ = VariantClear(&mut r);
        }
        s
    }

    /// Size of the monitor showing `hwnd` (physical pixels).
    fn monitor_size(hwnd: HWND) -> Option<(u32, u32)> {
        unsafe {
            let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            GetMonitorInfoW(mon, &mut mi).as_bool().then(|| {
                let r = mi.rcMonitor;
                ((r.right - r.left) as u32, (r.bottom - r.top) as u32)
            })
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
                let (disc, fatal) = (Arc::new(AtomicI32::new(0)), Arc::new(AtomicI32::new(0)));
                // Disconnection reasons only reach the container through events.
                let events = unk.cast::<IConnectionPointContainer>().ok().and_then(|cpc| {
                    let cp = cpc.FindConnectionPoint(&IMsTscAxEvents::IID).ok()?;
                    let sink: IMsTscAxEvents = Events {
                        disc: disc.clone(),
                        fatal: fatal.clone(),
                    }
                    .into();
                    let cookie = cp.Advise(&sink.cast::<IUnknown>().ok()?).ok()?;
                    Some((cp, cookie))
                });
                let mut session = RdpSession {
                    hwnd,
                    ax,
                    size: (p.width, p.height),
                    visible: false,
                    fit: p.resolution == Resolution::Fit,
                    disc,
                    fatal,
                    events,
                };
                session
                    .configure(&unk, p, HWND(parent as *mut c_void))
                    .map_err(|e| format!("configuration RDP : {e}"))?;
                invoke(&session.ax, "Connect", DISPATCH_METHOD, vec![]).map_err(|e| format!("connexion RDP : {e}"))?;
                Ok(session)
            }
        }

        fn configure(&mut self, unk: &IUnknown, p: &Params, parent: HWND) -> windows::core::Result<()> {
            let ax = &self.ax;
            let o = &p.opts;
            let full = p.resolution == Resolution::FullScreen;
            let (w, h) = match p.resolution {
                Resolution::Fixed => (o.width, o.height),
                Resolution::FullScreen => monitor_size(parent).unwrap_or((p.width, p.height)),
                _ => (p.width, p.height),
            };
            self.size = (w, h);
            put(ax, "Server", v_str(&p.host))?;
            if !p.username.is_empty() {
                put(ax, "UserName", v_str(&p.username))?;
            }
            if !p.domain.is_empty() {
                put(ax, "Domain", v_str(&p.domain))?;
            }
            put(ax, "DesktopWidth", v_i4(w.clamp(200, 8192) as i32))?;
            put(ax, "DesktopHeight", v_i4(h.clamp(200, 8192) as i32))?;
            let _ = put(ax, "ColorDepth", v_i4(o.depth() as i32));
            let _ = put(ax, "ConnectingText", v_str("Connexion en cours…"));
            let _ = put(ax, "DisconnectedText", v_str("Session déconnectée"));
            let adv = get_dispatch(ax, "AdvancedSettings9").or_else(|_| get_dispatch(ax, "AdvancedSettings2"))?;
            put(&adv, "RDPPort", v_i4(p.port as i32))?;
            let has_password = p.password.as_ref().is_some_and(|s| !s.is_empty());
            if let Some(pw) = p.password.as_ref().filter(|s| !s.is_empty()) {
                put(&adv, "ClearTextPassword", v_str(pw))?;
            }
            // Certificate warnings stay on unless the entry explicitly ignores them.
            let _ = put(&adv, "AuthenticationLevel", v_i4(if o.ignore_cert == Some(true) { 0 } else { 2 }));
            let nla = matches!(o.security.as_str(), "any" | "nla");
            let _ = put(&adv, "EnableCredSspSupport", v_bool(nla));
            let _ = put(&adv, "RedirectClipboard", v_bool(o.clipboard()));
            let _ = put(&adv, "RedirectDrives", v_bool(o.drives));
            let _ = put(&adv, "RedirectPrinters", v_bool(o.printers));
            let _ = put(&adv, "EnableAutoReconnect", v_bool(true));
            // Scales a fixed size to the tab, and the desktop after leaving full screen;
            // no effect in full screen (the window is the size of the monitor).
            let _ = put(&adv, "SmartSizing", v_bool(true));
            let _ = put(&adv, "GrabFocusOnConnect", v_bool(true));
            let mut perf = PERF_FONT_SMOOTHING | PERF_COMPOSITION;
            if !o.wallpaper {
                perf |= PERF_NO_WALLPAPER;
            }
            let _ = put(&adv, "PerformanceFlags", v_i4(perf));
            if o.console && put(&adv, "ConnectToAdministerServer", v_bool(true)).is_err() {
                let _ = put(&adv, "ConnectToServerConsole", v_bool(true));
            }
            if let Ok(sec) = get_dispatch(ax, "SecuredSettings2") {
                let _ = put(&sec, "AudioRedirectionMode", v_i4(o.audio.mode()));
                let _ = put(&sec, "KeyboardHookMode", v_i4(o.keyboard.mode()));
            }
            if o.gateway != Gateway::None {
                let ts = get_dispatch(ax, "TransportSettings2").or_else(|_| get_dispatch(ax, "TransportSettings"))?;
                put(&ts, "GatewayHostname", v_str(o.gateway_host.trim()))?;
                put(&ts, "GatewayUsageMethod", v_i4(o.gateway.usage_method()))?;
                let _ = put(&ts, "GatewayProfileUsageMethod", v_i4(1));
                // TSC_PROXY_CREDS_MODE_ANY; shared credentials reuse those of the session.
                let _ = put(&ts, "GatewayCredsSource", v_i4(4));
                let _ = put(&ts, "GatewayCredSharing", v_i4(i32::from(o.gateway_same_creds)));
            }
            // The control handles full screen itself (connection bar, "restore" back to the tab).
            let _ = put(&adv, "ContainerHandledFullScreen", v_i4(0));
            let _ = put(&adv, "DisplayConnectionBar", v_bool(true));
            let _ = put(&adv, "PinConnectionBar", v_bool(true));
            let _ = put(ax, "FullScreenTitle", v_str(&p.host));
            if full {
                if o.multi_mon {
                    let _ = put_nonscriptable_bool(unk, "UseMultimon", true);
                }
                let _ = put(ax, "FullScreen", v_bool(true));
            }
            // NLA (CredSSP) is only reachable through the non-scriptable interface.
            let _ = put_vtable_bool(unk, &IID_NONSCRIPTABLE3, VT_PUT_ENABLE_CREDSSP, nla);
            if o.security == "rdp" {
                let _ = put_nonscriptable_bool(unk, "NegotiateSecurityLayer", false);
            }
            // With a stored password, a refusal ends the session with its reason (banner
            // and "Reconnecter") instead of a Windows credential prompt.
            if has_password {
                let _ = put_nonscriptable_bool(unk, "AllowPromptingForCredentials", false);
            }
            Ok(())
        }

        pub fn state(&self) -> State {
            match get_i32(&self.ax, "Connected") {
                Ok(1) => State::Connected,
                Ok(2) => State::Connecting,
                _ => State::Disconnected,
            }
        }

        /// Readable reason of the last disconnection (`None`: normal end).
        pub fn disconnect_text(&self) -> Option<String> {
            let fatal = self.fatal.load(Ordering::Acquire);
            if fatal != 0 {
                return Some(format!("Erreur fatale du contrôle Bureau à distance (code {fatal})"));
            }
            let disc = self.disc.load(Ordering::Acquire);
            let ext = get_i32(&self.ax, "ExtendedDisconnectReason").unwrap_or(0);
            if let Some(t) = relay_desktop_core::conn::rdp_disconnect_text(disc, ext) {
                return Some(t);
            }
            if disc <= 3 {
                return None;
            }
            let desc = get_string(&self.ax, "GetErrorDescription", vec![v_i4(disc), v_i4(ext)]).filter(|s| !s.trim().is_empty());
            Some(match desc {
                Some(d) => format!("Déconnecté : {} (code {disc})", d.trim()),
                None => format!("Déconnecté (code {disc}, motif étendu {ext})"),
            })
        }

        /// Gives the keyboard focus to the session.
        pub fn focus(&self) {
            if self.visible {
                unsafe {
                    let _ = SetFocus(Some(self.hwnd));
                }
            }
        }

        /// Places the control over the tab area (client pixels) and shows/hides it;
        /// true when it has just been hidden (the area it covered must be repainted).
        pub fn place(&mut self, rect: (i32, i32, i32, i32), visible: bool) -> bool {
            let hidden = self.visible && !visible;
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
            hidden
        }

        /// Asks the server to adopt a new resolution (dynamic resolution, RDP 8.1+),
        /// only when the entry follows the tab size.
        pub fn resize_desktop(&mut self, width: u32, height: u32) {
            let (width, height) = (width.clamp(200, 8192) & !1, height.clamp(200, 8192));
            if !self.fit || (width, height) == self.size || self.state() != State::Connected {
                return;
            }
            if !self.full_screen() {
                self.update_display(width, height);
            }
        }

        fn update_display(&mut self, width: u32, height: u32) {
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

        fn full_screen(&self) -> bool {
            get_bool(&self.ax, "FullScreen").unwrap_or(false)
        }

        /// Switches a connected session to full screen (the connection bar brings it
        /// back to the tab); a session that follows the tab takes the monitor size.
        pub fn enter_full_screen(&mut self) -> bool {
            if self.state() != State::Connected || self.full_screen() {
                return false;
            }
            if self.fit {
                if let Some((w, h)) = monitor_size(self.hwnd) {
                    self.update_display(w & !1, h);
                }
            }
            put(&self.ax, "FullScreen", v_bool(true)).is_ok()
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
                if let Some((cp, cookie)) = self.events.take() {
                    let _ = cp.Unadvise(cookie);
                }
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
    pub fn disconnect_text(&self) -> Option<String> {
        None
    }
    pub fn focus(&self) {}
    pub fn place(&mut self, _: (i32, i32, i32, i32), _: bool) {}
    pub fn resize_desktop(&mut self, _: u32, _: u32) {}
    pub fn enter_full_screen(&mut self) -> bool {
        false
    }
    pub fn disconnect(&self) {}
}
