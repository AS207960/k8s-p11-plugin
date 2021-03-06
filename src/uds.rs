pub struct DeleteOnDrop {
    path: std::path::PathBuf,
    listener: tokio::net::UnixListener,
}

pub struct DeleteOnDropStream {
    stream: tokio::net::UnixStream
}

#[derive(Clone)]
pub struct DeleteOnDropConnectInfo {}

impl DeleteOnDrop {
    pub fn bind(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_owned();
        tokio::net::UnixListener::bind(&path).map(|listener| DeleteOnDrop { path, listener })
    }
}

impl Drop for DeleteOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl std::ops::Deref for DeleteOnDrop {
    type Target = tokio::net::UnixListener;

    fn deref(&self) -> &Self::Target {
        &self.listener
    }
}

impl std::ops::DerefMut for DeleteOnDrop {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.listener
    }
}

impl futures::stream::Stream for DeleteOnDrop {
    type Item = std::io::Result<DeleteOnDropStream>;

    fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        let socket = futures::ready!(self.poll_accept(cx))?;
        std::task::Poll::Ready(Some(Ok(DeleteOnDropStream {
            stream: socket.0
        })))
    }
}

impl tonic::transport::server::Connected for DeleteOnDropStream {
    type ConnectInfo = DeleteOnDropConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        DeleteOnDropConnectInfo {}
    }
}

impl tokio::io::AsyncWrite for DeleteOnDropStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        tokio::net::UnixStream::poll_write(std::pin::Pin::new(&mut self.get_mut().stream), cx, buf)
    }

    fn poll_flush(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        tokio::net::UnixStream::poll_flush(std::pin::Pin::new(&mut self.get_mut().stream), cx)
    }

    fn poll_shutdown(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        tokio::net::UnixStream::poll_shutdown(std::pin::Pin::new(&mut self.get_mut().stream), cx)
    }
}

impl tokio::io::AsyncRead for DeleteOnDropStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        tokio::net::UnixStream::poll_read(std::pin::Pin::new(&mut self.get_mut().stream), cx, buf)
    }
}

pub struct UnixStream {
    path: std::path::PathBuf,
}

impl UnixStream {
    pub fn connect<P: AsRef<std::path::Path>>(path: P) -> Self {
        Self { path: path.as_ref().to_path_buf() }
    }
}

impl tower_service::Service<tonic::transport::Uri> for UnixStream {
    type Response = tokio::net::UnixStream;
    type Error = std::io::Error;
    type Future = futures_util::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: tonic::transport::Uri) -> Self::Future {
        let path = self.path.clone();
        let fut = async move {
            tokio::net::UnixStream::connect(path).await
        };

        Box::pin(fut)
    }
}