//! UPnP Internet Gateway Device: SSDP discovery, then SOAP over a minimal
//! HTTP/1.1 client (std TcpStream, one request per connection).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::Event;
use quick_xml::Reader;

/// A discovered UPnP Internet Gateway Device.
#[derive(Debug)]
pub(super) struct Igd {
    control_url: String,
    service_type: String,
    local_ip: Option<IpAddr>,
}

const IGD_SERVICES: [&str; 3] = [
    "urn:schemas-upnp-org:service:WANIPConnection:1",
    "urn:schemas-upnp-org:service:WANIPConnection:2",
    "urn:schemas-upnp-org:service:WANPPPConnection:1",
];

/// Go's io.LimitReader(resp.Body, 1<<20): longer bodies are cut, not refused.
const BODY_LIMIT: u64 = 1 << 20;

/// Looks for an internet gateway with SSDP (M-SEARCH to the multicast address
/// 239.255.255.250:1900).
pub(super) fn discover_igd(timeout: Duration) -> anyhow::Result<Igd> {
    let c = UdpSocket::bind("0.0.0.0:0")?;
    let msg = "M-SEARCH * HTTP/1.1\r\n\
        HOST: 239.255.255.250:1900\r\n\
        MAN: \"ssdp:discover\"\r\n\
        MX: 2\r\n\
        ST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n";
    c.send_to(msg.as_bytes(), "239.255.255.250:1900")?;
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 2048];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bail!("upnp: no gateway found: i/o timeout");
        }
        c.set_read_timeout(Some(left))?;
        let (n, from) = c
            .recv_from(&mut buf)
            .map_err(|e| anyhow!("upnp: no gateway found: {e}"))?;
        let location = header_value(&String::from_utf8_lossy(&buf[..n]), "location");
        if location.is_empty() {
            continue;
        }
        let Ok(mut igd) = describe(&location) else {
            continue;
        };
        igd.local_ip = local_ip_towards(from.ip());
        return Ok(igd);
    }
}

fn header_value(resp: &str, key: &str) -> String {
    resp.split("\r\n")
        .filter_map(|line| line.split_once(':'))
        .find(|(k, _)| !k.is_empty() && k.trim().eq_ignore_ascii_case(key))
        .map_or(String::new(), |(_, v)| v.trim().to_string())
}

/// Fetches the device description and picks a WAN connection service.
fn describe(location: &str) -> anyhow::Result<Igd> {
    let (_, body) = http("GET", location, "", "", Duration::from_secs(5))?;
    // The description nests services several levels deep, so walk the tree
    // instead of relying on a fixed path.
    let mut found = Vec::new();
    services(&parse_xml(&body), &mut found);
    for want in IGD_SERVICES {
        if let Some((typ, ctl)) = found.iter().find(|(t, c)| t == want && !c.is_empty()) {
            return Ok(Igd {
                control_url: resolve_ref(location, ctl)?,
                service_type: typ.clone(),
                local_ip: None,
            });
        }
    }
    bail!("upnp: {location} exposes no WAN connection service")
}

/// Returns the local address used to reach the gateway.
fn local_ip_towards(gateway: IpAddr) -> Option<IpAddr> {
    let c = UdpSocket::bind("0.0.0.0:0").ok()?; // opened only to learn the route
    c.connect((gateway, 1900)).ok()?;
    Some(c.local_addr().ok()?.ip())
}

impl Igd {
    /// Issues one SOAP action and returns the response body.
    fn soap(&self, action: &str, body: &str) -> anyhow::Result<String> {
        let st = &self.service_type;
        let env = format!(
            "<?xml version=\"1.0\"?>\
             <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
             s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>\
             <u:{action} xmlns:u=\"{st}\">{body}</u:{action}>\
             </s:Body></s:Envelope>"
        );
        let headers = format!(
            "Content-Type: text/xml; charset=\"utf-8\"\r\nSOAPAction: \"{st}#{action}\"\r\n"
        );
        let (status, out) = http(
            "POST",
            &self.control_url,
            &headers,
            &env,
            Duration::from_secs(8),
        )?;
        if status.split(' ').next() != Some("200") {
            bail!("upnp: {action} returned {status}");
        }
        Ok(out)
    }

    /// Runs GetExternalIPAddress.
    pub(super) fn external_ip(&self) -> anyhow::Result<IpAddr> {
        let body = self.soap("GetExternalIPAddress", "")?;
        let raw = inner_text(&body, "NewExternalIPAddress");
        raw.trim()
            .parse::<IpAddr>()
            .map(|ip| ip.to_canonical())
            .map_err(|_| anyhow!("upnp: bad external ip {raw:?}"))
    }

    /// Forwards a public TCP port to us.
    pub(super) fn add_port_mapping(
        &self,
        port: u16,
        description: &str,
        lease: Duration,
    ) -> anyhow::Result<()> {
        let Some(local) = self.local_ip else {
            bail!("upnp: local address unknown")
        };
        let body = format!(
            "<NewRemoteHost></NewRemoteHost><NewExternalPort>{port}</NewExternalPort>\
             <NewProtocol>TCP</NewProtocol><NewInternalPort>{port}</NewInternalPort>\
             <NewInternalClient>{local}</NewInternalClient><NewEnabled>1</NewEnabled>\
             <NewPortMappingDescription>{description}</NewPortMappingDescription>\
             <NewLeaseDuration>{}</NewLeaseDuration>",
            lease.as_secs()
        );
        self.soap("AddPortMapping", &body).map(drop)
    }

    /// Removes a mapping added earlier.
    pub(super) fn delete_port_mapping(&self, port: u16) -> anyhow::Result<()> {
        let body = format!(
            "<NewRemoteHost></NewRemoteHost><NewExternalPort>{port}</NewExternalPort><NewProtocol>TCP</NewProtocol>"
        );
        self.soap("DeletePortMapping", &body).map(drop)
    }
}

// ---- HTTP/1.1 client ----

/// Splits "http://host[:port]/path" into ("host[:port]", "/path").
/// shortcut: plain http, no redirects (IGDs serve both on the LAN directly); add if a router needs them.
fn split_url(url: &str) -> anyhow::Result<(&str, &str)> {
    let rest = url
        .get(..7)
        .filter(|s| s.eq_ignore_ascii_case("http://"))
        .map(|_| &url[7..])
        .ok_or_else(|| anyhow!("upnp: unsupported URL {url:?}"))?;
    let rest = rest.split('#').next().unwrap_or_default();
    let (host, path) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if host.is_empty() {
        bail!("upnp: no host in URL {url:?}");
    }
    Ok((host, path))
}

/// Resolves a controlURL against the description's location (Go's
/// url.ResolveReference). shortcut: no "." / ".." segment cleanup; routers send
/// absolute paths or plain names.
fn resolve_ref(base: &str, r: &str) -> anyhow::Result<String> {
    let (host, path) = split_url(base)?;
    Ok(if r.contains("://") {
        r.to_string()
    } else if let Some(rest) = r.strip_prefix("//") {
        format!("http://{rest}")
    } else if r.starts_with('/') {
        format!("http://{host}{r}")
    } else {
        let dir = path
            .split('?')
            .next()
            .filter(|d| !d.is_empty())
            .unwrap_or("/");
        format!("http://{host}{}{r}", &dir[..=dir.rfind('/').unwrap_or(0)])
    })
}

/// A socket whose every read is bounded by one deadline (Go's Client.Timeout).
struct Deadline(TcpStream, Instant);

impl Deadline {
    fn left(&self) -> io::Result<Duration> {
        let left = self.1.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "i/o timeout"));
        }
        Ok(left)
    }
}

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.left()?;
        self.0.set_read_timeout(Some(left))?;
        self.0.read(buf)
    }
}

/// One CRLF-terminated line (at most 64 KiB), without the line ending.
fn read_line(r: &mut impl BufRead) -> io::Result<String> {
    let mut l = String::new();
    r.take(64 << 10).read_line(&mut l)?;
    if !l.ends_with('\n') {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(l.trim_end_matches(['\r', '\n']).to_string())
}

/// Reads exactly n bytes (or BODY_LIMIT, whichever is reached first) onto body.
fn read_n(r: &mut impl Read, n: u64, body: &mut Vec<u8>) -> io::Result<()> {
    let want = n.min(BODY_LIMIT - body.len() as u64);
    if r.take(want).read_to_end(body)? as u64 != want {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(())
}

/// Sends one request on a fresh connection (Connection: close) and returns the
/// status ("200 OK") and up to 1 MiB of body. headers: extra "K: V\r\n" lines.
fn http(
    method: &str,
    url: &str,
    headers: &str,
    body: &str,
    timeout: Duration,
) -> anyhow::Result<(String, String)> {
    let deadline = Instant::now() + timeout;
    let (host, path) = split_url(url)?;
    let target = if host.ends_with(']') || !host.contains(':') {
        format!("{host}:80")
    } else {
        host.to_string()
    };
    let mut last = anyhow!("upnp: cannot resolve {host}");
    let mut stream = None;
    for addr in target.to_socket_addrs()? {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        match TcpStream::connect_timeout(&addr, left) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = e.into(),
        }
    }
    let mut d = Deadline(stream.ok_or(last)?, deadline);

    let length = if method == "GET" {
        String::new()
    } else {
        format!("Content-Length: {}\r\n", body.len())
    };
    let req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n{length}{headers}\r\n{body}");
    d.0.set_write_timeout(Some(d.left()?))?;
    d.0.write_all(req.as_bytes())?;

    let mut r = BufReader::new(d);
    let status_line = read_line(&mut r)?;
    let status = match status_line.split_once(' ') {
        Some((proto, status)) if proto.starts_with("HTTP/") => status.trim().to_string(),
        _ => bail!("upnp: malformed HTTP response {status_line:?}"),
    };
    let (mut content_length, mut chunked) = (None, false);
    loop {
        let line = read_line(&mut r)?;
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = Some(
                    v.trim()
                        .parse::<u64>()
                        .map_err(|_| anyhow!("upnp: bad Content-Length {v:?}"))?,
                );
            } else if k.trim().eq_ignore_ascii_case("transfer-encoding") {
                chunked = v.to_ascii_lowercase().contains("chunked");
            }
        }
    }

    let mut out = Vec::new();
    if chunked {
        while (out.len() as u64) < BODY_LIMIT {
            let line = read_line(&mut r)?;
            let size = line.split(';').next().unwrap_or_default().trim();
            let size = u64::from_str_radix(size, 16)
                .map_err(|_| anyhow!("upnp: bad chunk size {line:?}"))?;
            if size == 0 {
                break;
            }
            read_n(&mut r, size, &mut out)?;
            if out.len() as u64 >= BODY_LIMIT {
                break; // cut mid-chunk, like Go's LimitReader
            }
            read_line(&mut r)?; // CRLF after the chunk
        }
    } else if let Some(n) = content_length {
        read_n(&mut r, n, &mut out)?;
    } else {
        r.take(BODY_LIMIT).read_to_end(&mut out)?;
    }
    Ok((status, String::from_utf8_lossy(&out).into_owned()))
}

// ---- XML ----

/// One element: its namespace-local name, its own character data (entities
/// resolved, CDATA included) and its child elements.
#[derive(Default)]
struct Node {
    name: String,
    text: String,
    kids: Vec<Node>,
}

/// Parses as much of doc as is well-formed: elements closed before an error or
/// EOF are kept, unclosed ones dropped (Go's token loop + DecodeElement).
fn parse_xml(doc: &str) -> Node {
    // Closes the innermost open element into its parent; the root stays.
    fn close(stack: &mut Vec<Node>) {
        if stack.len() > 1 {
            let n = stack.pop().expect("len > 1");
            stack.last_mut().expect("root").kids.push(n);
        }
    }
    let mut r = Reader::from_str(doc);
    let mut stack = vec![Node::default()];
    loop {
        let ev = r.read_event();
        let top = stack.last_mut().expect("root stays on the stack");
        match ev {
            Ok(Event::Start(e)) => stack.push(Node {
                name: e.local_name().as_ref().to_string(),
                ..Default::default()
            }),
            Ok(Event::Empty(e)) => top.kids.push(Node {
                name: e.local_name().as_ref().to_string(),
                ..Default::default()
            }),
            Ok(Event::End(_)) => close(&mut stack),
            Ok(Event::Text(t)) => top.text.push_str(&t.xml10_content()),
            Ok(Event::CData(t)) => top.text.push_str(&t.xml10_content()),
            Ok(Event::GeneralRef(e)) => match e.resolve_char_ref() {
                Ok(Some(c)) => top.text.push(c),
                _ => top
                    .text
                    .push_str(resolve_xml_entity(&e.into_inner()).unwrap_or_default()),
            },
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    while stack.len() > 1 {
        let n = stack.pop().expect("len > 1");
        stack.last_mut().expect("root").kids.extend(n.kids);
    }
    stack.pop().expect("root")
}

/// Every <service> element as (serviceType, controlURL); a field repeated
/// inside one service takes its last value, as Go's DecodeElement did.
fn services(n: &Node, out: &mut Vec<(String, String)>) {
    for k in &n.kids {
        if k.name != "service" {
            services(k, out);
            continue;
        }
        let field = |f: &str| {
            k.kids
                .iter()
                .rev()
                .find(|c| c.name == f)
                .map_or(String::new(), |c| c.text.clone())
        };
        out.push((field("serviceType"), field("controlURL")));
    }
}

/// The character data of the first element named tag (any namespace).
fn inner_text(doc: &str, tag: &str) -> String {
    fn find<'a>(n: &'a Node, tag: &str) -> Option<&'a Node> {
        if n.name == tag {
            return Some(n);
        }
        n.kids.iter().find_map(|k| find(k, tag))
    }
    find(&parse_xml(doc), tag).map_or(String::new(), |n| n.text.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    /// Serves one connection: records the request head (+ body for POST), sends
    /// resp, then waits for the client to hang up (so only Content-Length or
    /// the chunk terminator can end the body, unless close_after is set).
    fn serve(resp: impl Into<Vec<u8>>, close_after: bool) -> (String, thread::JoinHandle<String>) {
        let resp = resp.into();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let h = thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut req = Vec::new();
            let mut b = [0u8; 4096];
            while !String::from_utf8_lossy(&req).contains("</s:Envelope>")
                && (!req.ends_with(b"\r\n\r\n") || !req.starts_with(b"GET"))
            {
                let n = s.read(&mut b).unwrap();
                assert!(n > 0, "client hung up mid-request");
                req.extend_from_slice(&b[..n]);
            }
            s.write_all(&resp).unwrap();
            if !close_after {
                let _ = s.read(&mut b); // EOF once the client is done
            }
            String::from_utf8(req).unwrap()
        });
        (base, h)
    }

    const DESC: &str = "<?xml version=\"1.0\"?>\
        <root xmlns=\"urn:schemas-upnp-org:device-1-0\"><device><deviceList><device><serviceList>\
        <service><serviceType>urn:schemas-upnp-org:service:WANCommonInterfaceConfig:1</serviceType>\
        <controlURL>/common</controlURL></service>\
        <d:service xmlns:d=\"urn:x\"><d:serviceType>urn:schemas-upnp-org:service:WANPPPConnection:1</d:serviceType>\
        <d:controlURL>ppp</d:controlURL></d:service>\
        <service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>\
        <controlURL><![CDATA[ctl/]]>ip?a=1&amp;b=2</controlURL></service>\
        </serviceList></device></deviceList></device></root>";

    #[test]
    fn test_describe_chunked_relative_control_url() {
        // DESC split into two chunks, with a chunk extension.
        let (a, b) = DESC.split_at(100);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x};x=y\r\n{a}\r\n{:x}\r\n{b}\r\n0\r\n\r\n",
            a.len(),
            b.len()
        );
        let (base, h) = serve(resp, false);
        let igd = describe(&format!("{base}/dev/desc.xml")).unwrap();
        assert_eq!(
            igd.service_type,
            "urn:schemas-upnp-org:service:WANIPConnection:1"
        );
        assert_eq!(igd.control_url, format!("{base}/dev/ctl/ip?a=1&b=2"));
        let req = h.join().unwrap();
        assert!(req.starts_with("GET /dev/desc.xml HTTP/1.1\r\n"), "{req}");
        assert!(req.contains("Connection: close\r\n"), "{req}");
    }

    #[test]
    fn test_soap_content_length_and_status() {
        let env = "<s:Envelope xmlns:s=\"x\"><s:Body><u:R xmlns:u=\"y\"><NewExternalIPAddress> 45.154.88.66 \
              </NewExternalIPAddress></u:R></s:Body></s:Envelope>";
        let (base, h) = serve(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{env}",
                env.len()
            ),
            false,
        );
        let igd = Igd {
            control_url: format!("{base}/ctl"),
            service_type: IGD_SERVICES[0].into(),
            local_ip: None,
        };
        assert_eq!(igd.external_ip().unwrap().to_string(), "45.154.88.66");
        let req = h.join().unwrap();
        assert!(
            req.contains(&format!(
                "SOAPAction: \"{}#GetExternalIPAddress\"\r\n",
                IGD_SERVICES[0]
            )),
            "{req}"
        );
        assert!(req.contains(
            "<u:GetExternalIPAddress xmlns:u=\"urn:schemas-upnp-org:service:WANIPConnection:1\">"
        ));

        let (base, h) = serve(b"HTTP/1.1 500 Internal Server Error\r\n\r\n<fault/>", true);
        let igd = Igd {
            control_url: format!("{base}/ctl"),
            service_type: IGD_SERVICES[0].into(),
            local_ip: None,
        };
        let e = igd.delete_port_mapping(14250).unwrap_err();
        assert_eq!(
            e.to_string(),
            "upnp: DeletePortMapping returned 500 Internal Server Error"
        );
        assert!(h
            .join()
            .unwrap()
            .contains("<NewExternalPort>14250</NewExternalPort>"));
        let e = igd
            .add_port_mapping(14250, "wartales-mp", Duration::from_secs(7200))
            .unwrap_err();
        assert_eq!(e.to_string(), "upnp: local address unknown");
    }

    #[test]
    fn test_http_read_to_eof_and_limits() {
        let (base, h) = serve(b"HTTP/1.0 200 OK\r\nServer: x\r\n\r\nall of it", true);
        let (status, body) =
            http("GET", &format!("{base}/"), "", "", Duration::from_secs(5)).unwrap();
        assert_eq!((status.as_str(), body.as_str()), ("200 OK", "all of it"));
        h.join().unwrap();

        // A short Content-Length body is an error, as in Go (unexpected EOF).
        let (base, h) = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nshort", true);
        assert!(http("GET", &format!("{base}/x"), "", "", Duration::from_secs(5)).is_err());
        h.join().unwrap();

        // A silent server hits the deadline instead of hanging.
        let (base, h) = serve(b"", false);
        let t = Instant::now();
        assert!(http(
            "GET",
            &format!("{base}/"),
            "",
            "",
            Duration::from_millis(300)
        )
        .is_err());
        assert!(t.elapsed() < Duration::from_secs(3));
        h.join().unwrap();
    }

    #[test]
    fn test_url_helpers() {
        let b = "http://192.168.1.1:5000/rootDesc.xml";
        assert_eq!(
            resolve_ref(b, "/ctl/IPConn").unwrap(),
            "http://192.168.1.1:5000/ctl/IPConn"
        );
        assert_eq!(
            resolve_ref(b, "ctl").unwrap(),
            "http://192.168.1.1:5000/ctl"
        );
        assert_eq!(
            resolve_ref(b, "http://10.0.0.1/x").unwrap(),
            "http://10.0.0.1/x"
        );
        assert_eq!(
            resolve_ref(b, "//10.0.0.1:80/y").unwrap(),
            "http://10.0.0.1:80/y"
        );
        assert_eq!(
            resolve_ref("http://h/a/b?q=1/2", "c").unwrap(),
            "http://h/a/c"
        );
        assert_eq!(split_url("HTTP://h:1?x#f").unwrap(), ("h:1", "?x"));
        assert!(split_url("https://h/").is_err());
        let ssdp =
            "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\nLocation:  http://h:1/d.xml \r\n\r\n";
        assert_eq!(header_value(ssdp, "location"), "http://h:1/d.xml");
        assert_eq!(header_value(ssdp, "st"), "");
    }

    #[test]
    fn test_inner_text_namespaces_entities() {
        let doc = "<s:Envelope xmlns:s=\"e\"><s:Body><u:R xmlns:u=\"y\">\
            <u:NewExternalIPAddress>1.2.&#51;.4&lt;</u:NewExternalIPAddress></u:R></s:Body></s:Envelope>";
        assert_eq!(inner_text(doc, "NewExternalIPAddress"), "1.2.3.4<");
        assert_eq!(inner_text(doc, "Missing"), "");
        // Malformed tail: what was read before the error still counts.
        assert_eq!(inner_text("<a><b>x</b></c>", "b"), "x");
        assert_eq!(inner_text("<a><b>8.8.8.8", "b"), "");
        assert_eq!(resolve_ref("http://h?x", "ctl").unwrap(), "http://h/ctl");
    }
}
