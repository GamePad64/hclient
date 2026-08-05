use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Connect,
    FirstByte,
    BetweenBytes,
    Total,
}

/// Категория ошибки. Существует, чтобы потребителю не приходилось
/// классифицировать ошибки подстрочным матчингом по `Display`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    Resolve,
    Connect,
    Tls,
    Redirect,
    Timeout(Phase),
    Body,
    Decode,
    Status,
    Unsupported,
    Other,
}

/// `Clone` намеренно: непрозрачная и неклонируемая ошибка reqwest — источник
/// постоянных жалоб (reqwest#1053). `Arc<dyn Error>` не требует `Send`,
/// поэтому auto-trait прозрачность доходит и до ошибок.
#[derive(Debug, Clone)]
pub struct Error {
    kind: ErrorKind,
    source: Arc<dyn std::error::Error + 'static>,
}

impl Error {
    pub fn new<E: std::error::Error + 'static>(kind: ErrorKind, source: E) -> Self {
        Self {
            kind,
            source: Arc::new(source),
        }
    }
    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }
    pub fn is_timeout(&self) -> bool {
        matches!(self.kind, ErrorKind::Timeout(_))
    }
    pub fn is_redirect(&self) -> bool {
        matches!(self.kind, ErrorKind::Redirect)
    }
    pub fn is_connect(&self) -> bool {
        matches!(self.kind, ErrorKind::Connect)
    }
    pub fn is_unsupported(&self) -> bool {
        matches!(self.kind, ErrorKind::Unsupported)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.source)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Src;
    impl std::fmt::Display for Src {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "boom")
        }
    }
    impl std::error::Error for Src {}

    #[test]
    fn preserves_kind_and_source_without_stringifying() {
        let e = Error::new(ErrorKind::Resolve, Src);
        assert_eq!(e.kind(), &ErrorKind::Resolve);
        // Источник доступен целиком — не подстрокой сообщения.
        let src = std::error::Error::source(&e).unwrap();
        assert!(src.downcast_ref::<Src>().is_some());
    }

    #[test]
    fn is_clone_which_reqwest_error_is_not() {
        let e = Error::new(ErrorKind::Connect, Src);
        let c = e.clone();
        assert_eq!(c.kind(), &ErrorKind::Connect);
    }

    #[test]
    fn predicates_agree_with_kind() {
        assert!(Error::new(ErrorKind::Timeout(Phase::Connect), Src).is_timeout());
        assert!(Error::new(ErrorKind::Redirect, Src).is_redirect());
        assert!(!Error::new(ErrorKind::Body, Src).is_connect());
    }

    #[test]
    fn error_is_not_forced_send() {
        // Ядро не объявляет Send: ошибка от !Send-источника всё равно строится.
        // Поле не читается — оно нужно только своим типом, чтобы сделать
        // структуру !Send; отсюда allow(dead_code).
        #[allow(dead_code)]
        struct NotSend(std::rc::Rc<()>);
        impl std::fmt::Debug for NotSend {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "ns")
            }
        }
        impl std::fmt::Display for NotSend {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "ns")
            }
        }
        impl std::error::Error for NotSend {}
        let _ = Error::new(ErrorKind::Other, NotSend(std::rc::Rc::new(())));
    }
}
