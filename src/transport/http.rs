use super::{Request, Response, SadtTransport, TransportError};
use reqwest::blocking::{Client, ClientBuilder};
use reqwest::{Certificate, Method, Url};
use std::collections::BTreeMap;
#[cfg(feature = "test-support")]
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

const SESSION_HEADER: &str = "X-sap-adt-sessiontype";
const DISCOVERY_URI: &str = "/sap/bc/adt/discovery";
const CONTEXT_COOKIE: &str = "sap-contextid";

pub struct HttpConfig {
    pub base_url: String,
    pub client: String,
    pub username: String,
    pub password: String,
    pub ca_file: Option<PathBuf>,
    pub insecure: bool,
    pub timeout: Duration,
}

pub struct HttpTransport {
    client: Client,
    base_url: Url,
    client_id: String,
    username: String,
    password: String,
    csrf_token: Option<String>,
    session_open: bool,
    /// `None` until a stateful reply is seen; then whether any reply set the context cookie.
    stateful_supported: Option<bool>,
    #[cfg(feature = "test-support")]
    replies: Option<VecDeque<Result<Response, TransportError>>>,
    #[cfg(feature = "test-support")]
    recorded: Vec<Request>,
}

impl HttpTransport {
    pub fn connect(config: HttpConfig) -> Result<Self, TransportError> {
        // The cookie store keeps every Set-Cookie value, so the stateful ADT session survives.
        // A redirect would re-send the request (with its lock handle) elsewhere: never follow.
        let mut builder = ClientBuilder::new()
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .danger_accept_invalid_certs(config.insecure)
            .connect_timeout(config.timeout)
            .timeout(config.timeout);

        if let Some(path) = config.ca_file {
            let pem = fs::read(path)
                .map_err(|error| TransportError::PreSendConnectivity(error.to_string()))?;
            let certificate = Certificate::from_pem(&pem)
                .map_err(|error| TransportError::PreSendConnectivity(error.to_string()))?;
            builder = builder.add_root_certificate(certificate);
        }

        let client = builder
            .build()
            .map_err(|error| TransportError::PreSendConnectivity(error.to_string()))?;
        let base_url = Url::parse(&config.base_url)
            .map_err(|error| TransportError::PreSendConnectivity(error.to_string()))?;

        Ok(Self {
            client,
            base_url,
            client_id: config.client,
            username: config.username,
            password: config.password,
            csrf_token: None,
            session_open: false,
            stateful_supported: None,
            #[cfg(feature = "test-support")]
            replies: None,
            #[cfg(feature = "test-support")]
            recorded: Vec::new(),
        })
    }

    #[cfg(feature = "test-support")]
    pub fn for_test(replies: Vec<Result<Response, TransportError>>) -> Self {
        Self {
            client: ClientBuilder::new()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("build test HTTP client"),
            base_url: Url::parse("https://example.com/").expect("parse test URL"),
            client_id: "100".to_owned(),
            username: "user".to_owned(),
            password: "password".to_owned(),
            csrf_token: None,
            session_open: false,
            stateful_supported: None,
            replies: Some(replies.into()),
            recorded: Vec::new(),
        }
    }

    #[cfg(feature = "test-support")]
    pub fn recorded(&self) -> &[Request] {
        &self.recorded
    }

    pub fn classify(status: u16, csrf_token: Option<&str>) -> TransportError {
        if status == 403 && csrf_token.is_some_and(|value| value.eq_ignore_ascii_case("Required")) {
            return TransportError::CsrfRequired;
        }

        TransportError::Http {
            status,
            message: "HTTP request failed".to_owned(),
        }
    }

    pub fn timeout_after_send() -> TransportError {
        TransportError::PostSendUncertain("HTTP timeout after request send".to_owned())
    }

    /// Only a failed connect or a request that could not be built proves nothing was sent.
    pub fn classify_send_error(error: &reqwest::Error) -> TransportError {
        if error.is_connect() || error.is_builder() {
            return TransportError::PreSendConnectivity(error.to_string());
        }

        if error.is_timeout() {
            return Self::timeout_after_send();
        }

        TransportError::PostSendUncertain(error.to_string())
    }

    fn header<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Some servers (seen on a 740 release) never set the context cookie: they answer the LOCK
    /// and then reject the following PUT with `423 ExceptionResourceInvalidLockHandle`.
    fn has_context_cookie(headers: &BTreeMap<String, String>) -> bool {
        headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case("set-cookie"))
            .any(|(_, value)| value.to_ascii_lowercase().contains(CONTEXT_COOKIE))
    }

    fn request_url(&self, uri: &str) -> Result<Url, TransportError> {
        // Append the client as text: re-encoding the query would alter encoded lock handles.
        let separator = if uri.contains('?') { '&' } else { '?' };
        let relative = format!(
            "{}{separator}sap-client={}",
            uri.trim_start_matches('/'),
            self.client_id
        );

        self.base_url
            .join(&relative)
            .map_err(|error| TransportError::PreSendConnectivity(error.to_string()))
    }

    fn prepare(&self, request: &Request, stateful: bool) -> Request {
        let mut prepared = request.clone();
        let session = if stateful { "stateful" } else { "stateless" };
        prepared
            .headers
            .insert(SESSION_HEADER.to_owned(), session.to_owned());

        if let Some(token) = &self.csrf_token {
            prepared
                .headers
                .insert("x-csrf-token".to_owned(), token.clone());
        }

        // Real requests get their cookies from the client's cookie store.
        #[cfg(feature = "test-support")]
        if self.replies.is_some() {
            prepared
                .headers
                .insert("Cookie".to_owned(), "sid=test".to_owned());
        }

        prepared
    }

    fn send_once(&mut self, request: &Request, stateful: bool) -> Result<Response, TransportError> {
        let prepared = self.prepare(request, stateful);

        #[cfg(feature = "test-support")]
        let response = if let Some(replies) = self.replies.as_mut() {
            self.recorded.push(prepared);
            replies.pop_front().unwrap_or_else(|| {
                Err(TransportError::PreSendConnectivity(
                    "no scripted HTTP reply".to_owned(),
                ))
            })
        } else {
            self.send_real(prepared)
        };

        #[cfg(not(feature = "test-support"))]
        let response = self.send_real(prepared);

        let response = response?;

        if stateful
            && !matches!(response.status, 401 | 403)
            && ((200..300).contains(&response.status)
                || Self::header(&response.headers, "set-cookie").is_some())
        {
            self.session_open = true;
        }

        // A cookie on any reply settles the capability; a later reply that leaves it out
        // never takes it back.
        if stateful {
            self.note_context_cookie(&response);
        }

        Ok(response)
    }

    /// Whether a lock can survive to the PUT. Sticky: one stateful reply that sets the context
    /// cookie is proof for the rest of the session, whatever status it carried. A reply without
    /// it only answers while nothing has been seen yet, so a server that never sets it is still
    /// found out before the LOCK.
    fn note_context_cookie(&mut self, response: &Response) {
        if Self::has_context_cookie(&response.headers) {
            self.stateful_supported = Some(true);
        } else if self.stateful_supported.is_none() && (200..300).contains(&response.status) {
            self.stateful_supported = Some(false);
        }
    }

    fn send_real(&self, prepared: Request) -> Result<Response, TransportError> {
        let method = Method::from_bytes(prepared.method.as_bytes()).map_err(|error| {
            TransportError::PreSendConnectivity(format!("invalid HTTP method: {error}"))
        })?;
        let mut builder = self
            .client
            .request(method, self.request_url(&prepared.uri)?)
            .basic_auth(&self.username, Some(&self.password))
            .body(prepared.body);

        for (name, value) in &prepared.headers {
            builder = builder.header(name, value);
        }

        let response = builder
            .send()
            .map_err(|error| Self::classify_send_error(&error))?;
        let status = response.status().as_u16();
        let mut headers = BTreeMap::new();
        // Every Set-Cookie arrives as its own header; joining them keeps the context cookie
        // visible after the map has only one entry per name.
        for (name, value) in response.headers() {
            let value = value.to_str().unwrap_or_default();
            headers
                .entry(name.as_str().to_owned())
                .and_modify(|existing: &mut String| {
                    existing.push_str("; ");
                    existing.push_str(value);
                })
                .or_insert_with(|| value.to_owned());
        }
        let body = response
            .bytes()
            .map_err(|error| TransportError::PostSendUncertain(error.to_string()))?
            .to_vec();

        Ok(Response {
            status,
            headers,
            body,
        })
    }

    fn refresh_csrf(&mut self) -> Result<(), TransportError> {
        self.csrf_token = None;
        let mut fetch = Request::get(DISCOVERY_URI);
        fetch
            .headers
            .insert("x-csrf-token".to_owned(), "Fetch".to_owned());
        let response = self.send_once(&fetch, true)?;
        let token = Self::header(&response.headers, "x-csrf-token")
            .filter(|value| !value.is_empty())
            .ok_or(TransportError::CsrfRequired)?;
        self.csrf_token = Some(token.to_owned());

        Ok(())
    }
}

impl SadtTransport for HttpTransport {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        let response = self.send_once(&request, true)?;

        if !Self::classify(
            response.status,
            Self::header(&response.headers, "x-csrf-token"),
        )
        .is_retryable_csrf()
        {
            return Ok(response);
        }

        self.refresh_csrf()?;
        let retry = self.send_once(&request, true)?;
        let error = Self::classify(retry.status, Self::header(&retry.headers, "x-csrf-token"));

        if error.is_retryable_csrf() {
            return Err(error);
        }

        Ok(retry)
    }

    /// Whether the server keeps the stateful session; `None` until a stateful reply arrives.
    fn stateful_supported(&self) -> Option<bool> {
        self.stateful_supported
    }

    /// Ends the stateful ADT session with one stateless request; the caller reports a failure.
    fn close(&mut self) -> Result<(), TransportError> {
        if !self.session_open {
            return Ok(());
        }

        // The session stays closed locally even when the request fails: no second attempt.
        self.session_open = false;
        self.csrf_token = None;
        self.send_once(&Request::get(DISCOVERY_URI), false)
            .map(|_| ())
    }
}
