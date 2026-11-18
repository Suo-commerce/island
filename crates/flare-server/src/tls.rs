//! The face over HTTPS (docs/server.md, "The face over HTTPS").
//!
//! A browser gives a page protected key storage and passkeys only on a
//! secure origin, so the page that is to approve must be served over TLS.
//! `flare-server` then listens twice: plainly, as before, for the
//! terminals, and with a certificate for the page. The certificate is for
//! a name that points at the server's tunnel address; getting and
//! renewing it is another program's work (an ACME client, by a DNS
//! record, since the server has no public web port). This module only
//! reads the files it is given, and reads them again when they change, so
//! a renewal needs no restart.

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

/// How long a client has to finish the TLS handshake.
pub const HANDSHAKE: Duration = Duration::from_secs(10);

/// How many finished handshakes may wait to be served.
const WAITING: usize = 64;

/// How a request reached the server: the scheme its page's origin has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    /// Over the plain listener.
    Plain,
    /// Over the TLS listener.
    Tls,
}

impl Scheme {
    /// The scheme of a page served this way.
    pub const fn page(self) -> &'static str {
        match self {
            Self::Plain => "http",
            Self::Tls => "https",
        }
    }

    /// The scheme of the session's WebSocket served this way.
    pub const fn socket(self) -> &'static str {
        match self {
            Self::Plain => "ws",
            Self::Tls => "wss",
        }
    }
}

/// The certificate chain and its key, as PEM files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Files {
    /// The certificate, then the chain above it.
    pub cert: PathBuf,
    /// The certificate's private key.
    pub key: PathBuf,
}

impl Files {
    /// The TLS configuration these files give.
    pub fn config(&self) -> Result<Arc<ServerConfig>, String> {
        let said = |path: &Path, why: &dyn std::fmt::Display| format!("{}: {why}", path.display());
        let certs = CertificateDer::pem_file_iter(&self.cert)
            .map_err(|error| said(&self.cert, &error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| said(&self.cert, &error))?;
        if certs.is_empty() {
            return Err(said(&self.cert, &"no certificate in it"));
        }
        let key =
            PrivateKeyDer::from_pem_file(&self.key).map_err(|error| said(&self.key, &error))?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| error.to_string())?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|error| said(&self.cert, &error))?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    }

    /// When the files last changed: the newer of the two.
    fn changed(&self) -> Option<SystemTime> {
        let of = |path: &Path| {
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .ok()
        };
        of(&self.cert).max(of(&self.key))
    }
}

/// A listener that hands out connections once their TLS handshake is
/// done. A handshake that stalls holds up no one else, and ends after
/// [`HANDSHAKE`].
pub struct Listener {
    address: SocketAddr,
    ready: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
}

impl Listener {
    /// Listen on `tcp` with the certificate in `files`. The files are
    /// read now, and again before a connection whenever they have
    /// changed; files that cannot be read then leave the certificate in
    /// use as it is.
    pub fn new(tcp: TcpListener, files: Files) -> Result<Self, String> {
        let address = tcp.local_addr().map_err(|error| error.to_string())?;
        let mut config = files.config()?;
        let mut read = files.changed();
        let (finished, ready) = mpsc::channel(WAITING);
        tokio::spawn(async move {
            // Until whoever serves is gone.
            while !finished.is_closed() {
                let Ok((stream, peer)) = tcp.accept().await else {
                    // Out of descriptors, most likely: wait, do not spin.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                };
                let now = files.changed();
                if now != read {
                    read = now;
                    match files.config() {
                        Ok(renewed) => {
                            config = renewed;
                            eprintln!("flare-server: the certificate was read again");
                        }
                        Err(why) => eprintln!(
                            "flare-server: the certificate changed and cannot be read, the old one stays: {why}"
                        ),
                    }
                }
                let acceptor = TlsAcceptor::from(config.clone());
                let handed = finished.clone();
                tokio::spawn(async move {
                    if let Ok(Ok(stream)) =
                        tokio::time::timeout(HANDSHAKE, acceptor.accept(stream)).await
                    {
                        let _ = handed.send((stream, peer)).await;
                    }
                });
            }
        });
        Ok(Self { address, ready })
    }
}

impl axum::serve::Listener for Listener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready.recv().await {
            Some(connection) => connection,
            // The accepting task is gone: nothing more will come.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.address)
    }
}
