//! Native RFB client: Proxmox consoles over the authenticated Relay WebSocket,
//! and direct VNC connections (TCP, VNC Authentication or no password).
use futures_util::{SinkExt, StreamExt};
use relay_desktop_core::source::PveConsole;
use slint::{Rgba8Pixel, SharedPixelBuffer};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
use vnc::{PixelFormat, VncConnector, VncEncoding, VncEvent, X11Event};

#[derive(Default)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}
impl Frame {
    pub fn image(&self) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
        (self.width > 0).then(|| SharedPixelBuffer::clone_from_slice(&self.pixels, self.width, self.height))
    }
    fn resize(&mut self, w: u16, h: u16) -> Result<(), String> {
        if w == 0 || h == 0 || w > 8192 || h > 8192 {
            return Err("Dimensions de console invalides".into());
        }
        self.width = w as u32;
        self.height = h as u32;
        self.pixels = vec![0; w as usize * h as usize * 4];
        Ok(())
    }
    /// CopyRect: the `src` area of the frame moves to `dst` (same size).
    fn copy(&mut self, dst: vnc::Rect, src: vnc::Rect) -> Result<(), String> {
        let (w, h) = (dst.width as usize, dst.height as usize);
        let fits = |r: &vnc::Rect| r.x as usize + w <= self.width as usize && r.y as usize + h <= self.height as usize;
        if !fits(&dst) || !fits(&src) {
            return Err("Rectangle VNC invalide".into());
        }
        let stride = self.width as usize * 4;
        // Overlapping areas: copy rows away from the destination first.
        let rows: Box<dyn Iterator<Item = usize>> = if dst.y > src.y { Box::new((0..h).rev()) } else { Box::new(0..h) };
        for row in rows {
            let s = (src.y as usize + row) * stride + src.x as usize * 4;
            let d = (dst.y as usize + row) * stride + dst.x as usize * 4;
            self.pixels.copy_within(s..s + w * 4, d);
        }
        Ok(())
    }
    fn raw(&mut self, r: vnc::Rect, data: &[u8]) -> Result<(), String> {
        if r.x as u32 + r.width as u32 > self.width
            || r.y as u32 + r.height as u32 > self.height
            || data.len() != r.width as usize * r.height as usize * 4
        {
            return Err("Rectangle VNC invalide".into());
        }
        for row in 0..r.height as usize {
            let dst = ((r.y as usize + row) * self.width as usize + r.x as usize) * 4;
            let src = row * r.width as usize * 4;
            self.pixels[dst..dst + r.width as usize * 4].copy_from_slice(&data[src..src + r.width as usize * 4]);
            for px in self.pixels[dst..dst + r.width as usize * 4].as_chunks_mut::<4>().0 {
                px[3] = 255;
            }
        }
        Ok(())
    }
}
/// Clipboard shared with the VNC server (RFB cut text, Latin-1, LF line
/// breaks): server text goes to the Windows clipboard, a new local text goes
/// to the server with the next key or pointer event.
#[derive(Default)]
struct ClipSync {
    /// Last text exchanged either way, in LF form.
    last: String,
    checked: Option<std::time::Instant>,
}

impl ClipSync {
    fn server_text(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n");
        if text == self.last {
            return;
        }
        self.last = text.clone();
        let _ = arboard::Clipboard::new().and_then(|mut c| c.set_text(text.replace('\n', "\r\n")));
    }

    /// Local clipboard text if it changed (checked at most 4 times a second).
    fn local_change(&mut self) -> Option<String> {
        if self.checked.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(250)) {
            return None;
        }
        self.checked = Some(std::time::Instant::now());
        let text = arboard::Clipboard::new().ok()?.get_text().ok()?.replace("\r\n", "\n");
        if text.is_empty() || text == self.last {
            return None;
        }
        self.last = text.clone();
        Some(text)
    }
}

pub struct Console {
    pub frame: Arc<Mutex<Frame>>,
    pub input: mpsc::UnboundedSender<X11Event>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Drop for Console {
    fn drop(&mut self) {
        if let Some(c) = self.cancel.take() {
            let _ = c.send(());
        }
    }
}
/// What the console connects to.
pub enum Target {
    Relay(PveConsole),
    Direct {
        host: String,
        port: u16,
        password: zeroize::Zeroizing<String>,
    },
}

pub fn spawn(target: Target, event: impl Fn(&'static str, String) + Send + 'static) -> Console {
    let frame = Arc::new(Mutex::new(Frame::default()));
    let f = frame.clone();
    let (tx, rx) = mpsc::unbounded_channel();
    let (cancel, stop) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            tokio::select! {
                result=run(target,f,rx,&event) => match result { Ok(())=>event("closed","Console terminée".into()), Err(e)=>event("error",e) },
                _=stop=>{}
            }
        });
    });
    Console {
        frame,
        input: tx,
        cancel: Some(cancel),
    }
}
async fn run(
    target: Target,
    frame: Arc<Mutex<Frame>>,
    input: mpsc::UnboundedReceiver<X11Event>,
    event: &(impl Fn(&'static str, String) + Send),
) -> Result<(), String> {
    let ticket = match target {
        Target::Relay(t) => t,
        Target::Direct { host, port, password } => {
            let tcp = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                tokio::net::TcpStream::connect((host.as_str(), port)),
            )
            .await
            .map_err(|_| format!("{host}:{port} : délai de connexion dépassé"))?
            .map_err(|e| format!("{host}:{port} : {e}"))?;
            let _ = tcp.set_nodelay(true);
            return rfb(tcp, password, frame, input, event).await;
        }
    };
    let mut req = ticket
        .url
        .as_str()
        .into_client_request()
        .map_err(|_| "Adresse de console invalide")?;
    req.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", ticket.bearer.as_str()).parse().map_err(|_| "Jeton invalide")?,
    );
    req.headers_mut().insert("Sec-WebSocket-Protocol", "binary".parse().unwrap());
    // Do not include tungstenite errors: their URLs can contain session tokens.
    let (ws, _) = tokio::time::timeout(std::time::Duration::from_secs(20), tokio_tungstenite::connect_async(req))
        .await
        .map_err(|_| "Délai du tunnel dépassé")?
        .map_err(|_| "Connexion au tunnel VNC refusée")?;
    let (stream, mut bridge) = tokio::io::duplex(256 * 1024);
    let (mut sink, mut source) = ws.split();
    let transport = tokio::spawn(async move {
        let mut buf = vec![0; 65536];
        loop {
            tokio::select! {
                n=bridge.read(&mut buf)=>{ let n=n.map_err(|_|())?; if n==0 {return Ok::<(),()>(());} sink.send(Message::Binary(buf[..n].to_vec().into())).await.map_err(|_|())?; },
                msg=source.next()=>match msg {
                    Some(Ok(Message::Binary(b)))=>bridge.write_all(&b).await.map_err(|_|())?,
                    Some(Ok(Message::Ping(b)))=>sink.send(Message::Pong(b)).await.map_err(|_|())?,
                    Some(Ok(Message::Close(_)))|None=>return Ok(()),
                    Some(Err(_))=>return Err(()), _=>{}
                }
            }
        }
    });
    // Guard aborts the detached transport on closure, errors and cancellation.
    struct Guard(tokio::task::JoinHandle<Result<(), ()>>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _guard = Guard(transport);
    rfb(stream, ticket.password, frame, input, event).await
}

/// RFB session on an established stream, until it ends or the tab closes.
async fn rfb<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + Sync + 'static>(
    stream: S,
    password: zeroize::Zeroizing<String>,
    frame: Arc<Mutex<Frame>>,
    mut input: mpsc::UnboundedReceiver<X11Event>,
    event: &(impl Fn(&'static str, String) + Send),
) -> Result<(), String> {
    let client = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        VncConnector::new(stream)
            .set_auth_method(async move { Ok(password.to_string()) })
            .set_pixel_format(PixelFormat::rgba())
            // Preferred first: compressed tiles, moved areas, then raw pixels.
            .add_encoding(VncEncoding::Zrle)
            .add_encoding(VncEncoding::Trle)
            .add_encoding(VncEncoding::CopyRect)
            .add_encoding(VncEncoding::Raw)
            .add_encoding(VncEncoding::DesktopSizePseudo)
            .allow_shared(true)
            .build()?
            .try_start()
            .await?
            .finish()
    })
    .await
    .map_err(|_| "Délai d'authentification VNC dépassé")?
    .map_err(|e| format!("Initialisation VNC refusée (mot de passe ou serveur incompatible) : {e}"))?;
    event("connected", String::new());
    let mut refresh = tokio::time::interval(std::time::Duration::from_millis(40));
    let mut clip = ClipSync::default();
    loop {
        tokio::select! {
            e=client.recv_event()=>match e.map_err(|_|"Console VNC interrompue")? {
                VncEvent::SetResolution(s)=>frame.lock().unwrap().resize(s.width,s.height)?,
                VncEvent::RawImage(r,b)=>frame.lock().unwrap().raw(r,&b)?,
                VncEvent::Copy(dst,src)=>frame.lock().unwrap().copy(dst,src)?,
                VncEvent::Text(t)=>clip.server_text(&t),
                VncEvent::Error(_)=>return Err("Erreur de console VNC".into()), _=>{}
            },
            e=input.recv()=>match e {
                Some(e)=>{
                    // The local clipboard goes to the server when the user acts in the session.
                    if matches!(e, X11Event::KeyEvent(_) | X11Event::PointerEvent(_)) {
                        if let Some(t) = clip.local_change() {
                            client.input(X11Event::CopyText(t)).await.map_err(|_|"Envoi VNC impossible")?;
                        }
                    }
                    client.input(e).await.map_err(|_|"Envoi VNC impossible")?
                }
                None=>{let _=client.close().await; return Ok(());}
            },
            _=refresh.tick()=>{client.input(X11Event::Refresh).await.map_err(|_|"Console interrompue")?;}
        }
    }
}

pub fn keysym(text: &str) -> Option<u32> {
    use slint::platform::Key;
    let special = [
        (Key::Return, 0xff0d),
        (Key::Backspace, 0xff08),
        (Key::Tab, 0xff09),
        (Key::Escape, 0xff1b),
        (Key::Delete, 0xffff),
        (Key::LeftArrow, 0xff51),
        (Key::UpArrow, 0xff52),
        (Key::RightArrow, 0xff53),
        (Key::DownArrow, 0xff54),
        (Key::Home, 0xff50),
        (Key::End, 0xff57),
        (Key::PageUp, 0xff55),
        (Key::PageDown, 0xff56),
        (Key::Shift, 0xffe1),
        (Key::Control, 0xffe3),
        (Key::Alt, 0xffe9),
    ];
    for (k, v) in special {
        if text == slint::SharedString::from(k).as_str() {
            return Some(v);
        }
    }
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() || c.is_control() {
        return None;
    }
    Some(if (c as u32) <= 255 {
        c as u32
    } else if (c as u32) >= 0xf700 && (c as u32) <= 0xf8ff {
        return None;
    } else {
        0x01000000 | c as u32
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rectangles_are_bounded() {
        let mut f = Frame::default();
        assert!(f.resize(0, 1).is_err());
        f.resize(2, 2).unwrap();
        let r = vnc::Rect {
            x: 1,
            y: 1,
            width: 1,
            height: 1,
        };
        f.raw(r, &[1, 2, 3, 0]).unwrap();
        assert_eq!(&f.pixels[12..16], &[1, 2, 3, 255]);
        assert!(f.raw(vnc::Rect { x: 2, ..r }, &[0; 4]).is_err());
        assert!(f.raw(r, &[0; 3]).is_err());
    }
    #[test]
    fn keyboard_mapping() {
        assert_eq!(keysym("a"), Some(97));
        assert_eq!(keysym("é"), Some(233));
        assert_eq!(keysym("\n"), Some(0xff0d));
        assert_eq!(keysym(&slint::SharedString::from(slint::platform::Key::Control)), Some(0xffe3));
    }
}

#[cfg(test)]
mod tunnel_tests {
    use super::*;
    // Exercises the HTTP upgrade, RFB handshake, framebuffer, keyboard and
    // cancellation without a real Proxmox host or device token.
    #[tokio::test]
    #[allow(clippy::result_large_err)] // tungstenite requires this callback signature
    async fn native_console_over_websocket() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/api/vnc-tunnel?token=once", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(
                socket,
                |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
                 mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    assert_eq!(req.headers()["Authorization"], "Bearer test-device");
                    assert_eq!(req.headers()["Sec-WebSocket-Protocol"], "binary");
                    response.headers_mut().insert("Sec-WebSocket-Protocol", "binary".parse().unwrap());
                    Ok(response)
                },
            )
            .await
            .unwrap();
            ws.send(Message::Binary(b"RFB 003.008\n".to_vec().into())).await.unwrap();
            assert_eq!(ws.next().await.unwrap().unwrap().into_data().as_ref(), b"RFB 003.008\n");
            ws.send(Message::Binary(vec![1, 1].into())).await.unwrap();
            assert_eq!(ws.next().await.unwrap().unwrap().into_data().as_ref(), &[1]);
            ws.send(Message::Binary(vec![0; 4].into())).await.unwrap();
            let _shared = ws.next().await.unwrap().unwrap();
            let mut init = vec![0, 2, 0, 2, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 0, 8, 16, 0, 0, 0];
            init.extend_from_slice(&[0, 0, 0, 4]);
            init.extend_from_slice(b"test");
            ws.send(Message::Binary(init.into())).await.unwrap();
            // Wait for the client's framebuffer request (type 3).
            loop {
                let b = ws.next().await.unwrap().unwrap().into_data();
                if b.first() == Some(&3) {
                    break;
                }
            }
            let mut frame = vec![0, 0, 0, 1, 0, 0, 0, 0, 0, 2, 0, 2, 0, 0, 0, 0];
            frame.extend_from_slice(&[11, 22, 33, 0].repeat(4));
            ws.send(Message::Binary(frame.into())).await.unwrap();
            loop {
                match ws.next().await {
                    Some(Ok(m)) => {
                        let b = m.into_data();
                        if b.first() == Some(&4) {
                            assert!(b.starts_with(&[4, 1, 0, 0, 0, 0, 0, 97]));
                            break;
                        }
                    }
                    _ => panic!("input missing"),
                }
            }
        });
        let ticket = PveConsole {
            url: zeroize::Zeroizing::new(url),
            bearer: zeroize::Zeroizing::new("test-device".into()),
            password: zeroize::Zeroizing::new(String::new()),
        };
        let console = spawn(Target::Relay(ticket), |_, _| {});
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if console.frame.lock().unwrap().pixels.first() == Some(&11) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(&console.frame.lock().unwrap().pixels[..4], &[11, 22, 33, 255]);
        console.input.send(X11Event::KeyEvent((97, true).into())).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        drop(console);
    }
}
