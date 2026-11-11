//! The server's certificate is always checked: against the operating
//! system's trust store, plus the company certificate file the person gave
//! for the server. Here a stand-in Gitea serves HTTPS with a certificate
//! from a certificate authority made up for the test, which no system
//! trusts.
//!
//! Not run on Windows, where this crate's TLS is the system's own
//! (SChannel) and the stand-in's TLS library is not built; the address
//! and file checks in `remote` are the same code on every system.

#![cfg(not(windows))]

mod support;

use std::net::SocketAddr;
use std::sync::Arc;

use columbus_sync::remote::{Address, Kind, Server};
use columbus_sync::Error;
use support::{http, put, Person, TOKEN};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

/// Accepts TCP connections and does the TLS handshake. A handshake the
/// client breaks off (it does not trust the certificate) never becomes a
/// connection, so no request of it reaches the server.
struct Tls {
    tcp: TcpListener,
    acceptor: TlsAcceptor,
}

impl axum::serve::Listener for Tls {
    type Io = tokio_rustls::server::TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let Ok((stream, addr)) = self.tcp.accept().await else {
                continue;
            };
            if let Ok(tls) = self.acceptor.accept(stream).await {
                return (tls, addr);
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

/// Valid from two days ago for two months: short, as a server
/// certificate must be for some systems to take it.
fn valid_now(p: &mut rcgen::CertificateParams) {
    use chrono::Datelike;
    let ymd = |offset: i64| {
        let d = chrono::Utc::now().date_naive() + chrono::Duration::days(offset);
        (d.year(), d.month() as u8, d.day() as u8)
    };
    let (y, m, d) = ymd(-2);
    p.not_before = rcgen::date_time_ymd(y, m, d);
    let (y, m, d) = ymd(60);
    p.not_after = rcgen::date_time_ymd(y, m, d);
}

/// A company's certificate authority, and a server certificate it signed
/// for `names`. Gives the authority's certificate as PEM, and what the
/// server needs.
fn company_ca(names: &[&str]) -> (String, Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
        KeyPair, KeyUsagePurpose,
    };
    let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.distinguished_name
        .push(DnType::OrganizationName, "Columbus test company");
    ca.distinguished_name
        .push(DnType::CommonName, "Columbus test company CA");
    ca.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    valid_now(&mut ca);
    let ca_key = KeyPair::generate().unwrap();
    let ca_cert = ca.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca, ca_key);

    let mut leaf =
        CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>()).unwrap();
    leaf.distinguished_name.push(DnType::CommonName, names[0]);
    leaf.use_authority_key_identifier_extension = true;
    leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    valid_now(&mut leaf);
    let leaf_key = KeyPair::generate().unwrap();
    let leaf_cert = leaf.signed_by(&leaf_key, &issuer).unwrap();
    (
        ca_cert.pem(),
        vec![leaf_cert.der().clone(), ca_cert.der().clone()],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
    )
}

struct Https {
    world: http::Shared,
    port: u16,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Https {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn https_gitea(chain: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>) -> Https {
    let world = http::shared();
    world.lock().unwrap().user("ana", "Ana Example", TOKEN);
    let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    let listener = Tls {
        tcp,
        acceptor: TlsAcceptor::from(Arc::new(config)),
    };
    let app = support::gitea::router(world.clone());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Https { world, port, task }
}

#[tokio::test]
async fn a_certificate_nobody_vouches_for_is_refused_and_the_companys_file_makes_it_trusted() {
    let (ca_pem, chain, key) = company_ca(&["localhost"]);
    let server = https_gitea(chain, key).await;
    server.world.lock().unwrap().repo("ana", "roads").commit(
        "main",
        &[put("a.md", b"a\n")],
        "start",
        "Cy",
    );
    let dir = tempfile::tempdir().unwrap();
    let address =
        Address::parse(Kind::Gitea, &format!("https://localhost:{}", server.port)).unwrap();

    // 1. Without the company's certificate: refused, in plain words, and
    //    no request (so no token) ever reached the server.
    let plain = Server::new(&address, TOKEN, None).unwrap();
    for err in [
        plain.check().await.unwrap_err(),
        plain.whoami().await.unwrap_err(),
    ] {
        assert!(matches!(err, Error::Network(_)), "{err:?}");
        let said = err.to_string();
        assert!(
            said.contains("certificate of localhost could not be checked"),
            "{said}"
        );
        assert!(
            said.contains("nothing was sent") && said.contains(".pem"),
            "{said}"
        );
        assert!(!said.contains("zq7k"));
    }
    assert_eq!(
        server.world.lock().unwrap().seen.len(),
        0,
        "nothing got past the handshake"
    );

    // 2. With another company's certificate: still refused.
    let (other_pem, _, _) = company_ca(&["localhost"]);
    let other = dir.path().join("other.pem");
    std::fs::write(&other, other_pem).unwrap();
    let err = Server::new(&address, TOKEN, Some(&other))
        .unwrap()
        .whoami()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Network(_)), "{err:?}");
    assert_eq!(server.world.lock().unwrap().seen.len(), 0);

    // 3. With this company's certificate file: trusted, and a sync works.
    let file = dir.path().join("company-ca.pem");
    std::fs::write(&file, &ca_pem).unwrap();
    let trusted = Server::new(&address, TOKEN, Some(&file)).unwrap();
    trusted.check().await.unwrap();
    assert_eq!(trusted.whoami().await.unwrap().login, "ana");
    let remote = trusted.remote("ana", "roads", "main").unwrap();
    let mut ana = Person::new("ana");
    ana.write("mine.md", "mine\n");
    let rep = ana.sync(&remote).await;
    assert_eq!((rep.got.len(), rep.sent.len()), (1, 1));
    assert_eq!(
        server
            .world
            .lock()
            .unwrap()
            .get("ana", "roads")
            .unwrap()
            .files("main")
            .len(),
        2
    );

    // 4. The name is checked too: the same server by another name (its
    //    address, which the certificate does not name) is refused even
    //    with the company's file.
    let by_number =
        Address::parse(Kind::Gitea, &format!("https://127.0.0.1:{}", server.port)).unwrap();
    let before = server.world.lock().unwrap().seen.len();
    let err = Server::new(&by_number, TOKEN, Some(&file))
        .unwrap()
        .whoami()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Network(_)), "{err:?}");
    assert_eq!(server.world.lock().unwrap().seen.len(), before);
}
