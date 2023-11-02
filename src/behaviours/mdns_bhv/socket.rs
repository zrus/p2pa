use std::{
  io::Error,
  marker::Unpin,
  net::{SocketAddr, UdpSocket},
  task::{Context, Poll},
};

/// Interface that must be implemented by the different runtimes to use the [`UdpSocket`] in async mode
#[allow(unreachable_pub)] // Users should not depend on this.
pub trait AsyncSocket: Unpin + Send + 'static {
  /// Create the async socket from the [`std::net::UdpSocket`]
  fn from_std(socket: UdpSocket) -> std::io::Result<Self>
  where
    Self: Sized;

  /// Attempts to receive a single packet on the socket from the remote address to which it is connected.
  fn poll_read(
    &mut self,
    _cx: &mut Context,
    _buf: &mut [u8],
  ) -> Poll<Result<(usize, SocketAddr), Error>>;

  /// Attempts to send data on the socket to a given address.
  fn poll_write(
    &mut self,
    _cx: &mut Context,
    _packet: &[u8],
    _to: SocketAddr,
  ) -> Poll<Result<(), Error>>;
}

use ::tokio::{io::ReadBuf, net::UdpSocket as TkUdpSocket};

/// Tokio ASync Socket`
pub(crate) type TokioUdpSocket = TkUdpSocket;
impl AsyncSocket for TokioUdpSocket {
  fn from_std(socket: UdpSocket) -> std::io::Result<Self> {
    socket.set_nonblocking(true)?;
    TokioUdpSocket::from_std(socket)
  }

  fn poll_read(
    &mut self,
    cx: &mut Context,
    buf: &mut [u8],
  ) -> Poll<Result<(usize, SocketAddr), Error>> {
    let mut rbuf = ReadBuf::new(buf);
    match self.poll_recv_from(cx, &mut rbuf) {
      Poll::Pending => Poll::Pending,
      Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
      Poll::Ready(Ok(addr)) => Poll::Ready(Ok((rbuf.filled().len(), addr))),
    }
  }

  fn poll_write(
    &mut self,
    cx: &mut Context,
    packet: &[u8],
    to: SocketAddr,
  ) -> Poll<Result<(), Error>> {
    match self.poll_send_to(cx, packet, to) {
      Poll::Pending => Poll::Pending,
      Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
      Poll::Ready(Ok(_len)) => Poll::Ready(Ok(())),
    }
  }
}
