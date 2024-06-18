pub type Result<T> = std::result::Result<T, Error>;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("IO Error: {0}")]
    IOError(#[from] std::io::Error),

    #[error("MDNS Lookup: {0}")]
    MDNSError(#[from] mdns_sd::Error),

    #[error("Flume Error: {0}")]
    FlumeError(#[from] flume::RecvError),

    #[error("JSON Decode Error")]
    JsonParseError(#[from] serde_json::Error),

    #[error("Error while processing response")]
    ProcessingResponseError,

    #[error("Server Error ({code}) {message}")]
    OSCError { code: u32, message: String },

    #[error("Reply was for a different path than the request. While acceptable according to specification, this library does currently not implement this")]
    UnexpectedPath,

    #[error("An SSC Path was specified, which is not syntactically valid")]
    InvalidPath,

    #[error("SSC Request timed out")]
    RequestTimeout,
}
