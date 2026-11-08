//! A small fake SAP for the runner cycle and its reconciliation (M6 Task 16): runner classes in
//! `$TMP`, the classrun POST with a scripted reply, and data previews of the DDIC tables whose
//! rows follow a `World` that may change with the time and with the POSTs that ran. No SAP.

use abapreach::transport::http::HttpTransport;
use abapreach::transport::{Request, Response, SadtTransport, TransportError, runner_verdict};
use std::cell::{Cell, RefCell};
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

pub const LOGOFF: &str = "/sap/public/bc/icf/logoff";
pub const OBJECT: &str = "YTEST_ABAPREACH_DT";
const LOCK_BODY: &str = concat!(
    r#"<asx:abap xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA>"#,
    r#"<LOCK_HANDLE>h+1/=</LOCK_HANDLE><CORRNR></CORRNR><IS_LOCAL>X</IS_LOCAL>"#,
    r#"</DATA></asx:values></asx:abap>"#,
);
const MESSAGES_OK: &str = r#"<chkl:messages xmlns:chkl="x"></chkl:messages>"#;

/// What the data previews see of the object.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct World {
    /// An active definition row.
    pub definition: bool,
    /// A TADIR row.
    pub directory: bool,
    /// An inactive definition row (`AS4LOCAL = 'N'`).
    pub inactive: bool,
    /// The previews fail with HTTP 500.
    pub broken: bool,
}

/// What a classrun POST answers.
#[derive(Clone, Debug)]
pub enum PostReply {
    /// The run executed and its text came back.
    Output(String),
    /// The run executed, then the reply is lost.
    Lost,
    /// HTTP status and body; the run executed.
    Status(u16, String),
}

/// What the world may depend on.
#[derive(Clone, Copy, Debug)]
pub struct Tick {
    /// The virtual time.
    pub elapsed: Duration,
    /// Classrun POSTs that ran.
    pub posts: usize,
    /// Data previews answered so far, the current one included.
    pub previews: usize,
}

/// The world as a function of the time elapsed and of what ran.
pub type Evolution = Box<dyn Fn(&Tick) -> World>;

pub struct Sap {
    pub classes: BTreeMap<String, String>,
    /// `(session, "METHOD uri")` of every request.
    pub log: Vec<(usize, String)>,
    pub sessions: usize,
    pub closes: usize,
    pub stateful: bool,
    pub posts: usize,
    /// Replies of the classrun POSTs in order; later POSTs answer an empty 200.
    pub post_replies: VecDeque<PostReply>,
    pub evolution: Evolution,
    /// The virtual time the evolution reads.
    pub now: Rc<Cell<Duration>>,
    /// DELETE of a class answers 500 and does nothing, this many times.
    pub failing_deletes: usize,
    pub previews: usize,
    /// The previews of the inactive version (`AS4LOCAL = 'N'`) answer HTTP 500.
    pub inactive_fails: bool,
    /// Runs when a classrun POST arrives, before it executes.
    pub on_post: Option<Box<dyn FnMut()>>,
    /// The source of the class each classrun POST ran, in order.
    pub posted: Vec<String>,
    /// Builds the output of a POST with no scripted reply from the class source it ran.
    pub reply_with: Option<ReplyWith>,
}

/// Builds the output of a classrun POST from the class source it ran.
pub type ReplyWith = Box<dyn Fn(&str) -> String>;

pub type Shared = Rc<RefCell<Sap>>;

pub fn new_sap(stateful: bool, now: &Rc<Cell<Duration>>, evolution: Evolution) -> Shared {
    Rc::new(RefCell::new(Sap {
        classes: BTreeMap::new(),
        log: Vec::new(),
        sessions: 0,
        closes: 0,
        stateful,
        posts: 0,
        post_replies: VecDeque::new(),
        evolution,
        now: now.clone(),
        failing_deletes: 0,
        previews: 0,
        inactive_fails: false,
        on_post: None,
        posted: Vec::new(),
        reply_with: None,
    }))
}

impl Sap {
    pub fn world(&self) -> World {
        (self.evolution)(&Tick {
            elapsed: self.now.get(),
            posts: self.posts,
            previews: self.previews,
        })
    }

    pub fn count(&self, needle: &str) -> usize {
        self.log
            .iter()
            .filter(|(_, line)| line.contains(needle))
            .count()
    }

    fn handle(
        &mut self,
        session: usize,
        request: Request,
        admit: bool,
    ) -> Result<Response, TransportError> {
        self.log
            .push((session, format!("{} {}", request.method, request.uri)));

        if admit && runner_verdict(&request) == Some(false) {
            return Err(TransportError::RequestNotAllowed(request.uri));
        }

        let (path, query) = request
            .uri
            .split_once('?')
            .unwrap_or((request.uri.as_str(), ""));
        let name = path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let method = request.method.as_str();

        Ok(match (method, path) {
            ("GET", LOGOFF) => Response::text(200, ""),
            ("GET", "/sap/bc/adt/discovery") => self.reply(Response::text(200, "<intf/>")),
            ("POST", "/sap/bc/adt/datapreview/ddic") => {
                let body = String::from_utf8_lossy(&request.body).into_owned();

                self.preview(&body)
            }
            ("POST", "/sap/bc/adt/oo/classes") => {
                let body = String::from_utf8_lossy(&request.body).into_owned();
                let name = body
                    .split("adtcore:name=\"")
                    .nth(1)
                    .and_then(|rest| rest.split('"').next())
                    .expect("name in create body")
                    .to_owned();

                match self.classes.entry(name) {
                    Entry::Occupied(_) => self.reply(Response::text(409, "exists")),
                    Entry::Vacant(slot) => {
                        slot.insert(String::new());
                        self.reply(Response::text(201, ""))
                    }
                }
            }
            ("POST", "/sap/bc/adt/activation") => self.reply(Response::text(200, MESSAGES_OK)),
            ("POST", path) if path.starts_with("/sap/bc/adt/oo/classrun/") => {
                self.posts += 1;
                self.posted
                    .push(self.classes.get(&name).cloned().unwrap_or_default());

                if let Some(hook) = self.on_post.as_mut() {
                    hook();
                }

                match self.post_replies.pop_front() {
                    Some(PostReply::Output(text)) => self.reply(Response::text(200, text)),
                    Some(PostReply::Status(status, body)) => {
                        self.reply(Response::text(status, body))
                    }
                    Some(PostReply::Lost) => {
                        return Err(TransportError::PostSendUncertain("reply lost".to_owned()));
                    }
                    None => {
                        let text = match (&self.reply_with, self.posted.last()) {
                            (Some(build), Some(source)) => build(source),
                            _ => String::new(),
                        };

                        self.reply(Response::text(200, text))
                    }
                }
            }
            ("POST", _) if query.contains("_action=LOCK") => {
                self.reply(Response::text(200, LOCK_BODY))
            }
            ("POST", _) if query.contains("_action=UNLOCK") => self.reply(Response::text(200, "")),
            ("PUT", _) => {
                if let Some(class) = self.classes.get_mut(&name_of_source(path)) {
                    *class = String::from_utf8_lossy(&request.body).into_owned();
                }

                self.reply(Response::text(200, ""))
            }
            ("DELETE", _) => {
                if self.failing_deletes > 0 {
                    self.failing_deletes -= 1;

                    return Ok(self.reply(Response::text(500, "delete failed")));
                }

                self.classes.remove(&name);
                self.reply(Response::text(200, ""))
            }
            ("GET", path) if path.ends_with("/source/main") => {
                match self.classes.get(&name_of_source(path)) {
                    Some(source) => self.reply(Response::text(200, source)),
                    None => self.reply(Response::text(404, "")),
                }
            }
            ("GET", path) if path.starts_with("/sap/bc/adt/oo/classes/") => {
                if self.classes.contains_key(&name) {
                    self.reply(Response::text(200, "<class/>"))
                } else {
                    self.reply(Response::text(404, ""))
                }
            }
            _ => panic!("unexpected request {request:?}"),
        })
    }

    /// A stateful server sets the context cookie on every reply.
    fn reply(&self, mut response: Response) -> Response {
        if self.stateful {
            response
                .headers
                .insert("Set-Cookie".to_owned(), "sap-contextid=ctx".to_owned());
        }

        response
    }

    fn preview(&mut self, body: &str) -> Response {
        use super::preview;

        self.previews += 1;

        let world = self.world();
        let none: &[Option<&str>] = &[];
        let table = body
            .split(" FROM ")
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .unwrap_or_default();
        let fields: Vec<&str> = body
            .trim_start_matches("SELECT ")
            .split(" FROM ")
            .next()
            .unwrap_or_default()
            .split(", ")
            .collect();
        let quoted: Vec<&str> = body.split('\'').skip(1).step_by(2).collect();

        if (world.broken || (self.inactive_fails && body.contains("AS4LOCAL = 'N'")))
            && !matches!(table, "T000" | "TRNSPACE")
        {
            return Response::text(500, "preview failed");
        }

        let (present, key) = match table {
            "T000" => {
                return Response::text(
                    200,
                    preview(&[
                        ("CCCATEGORY", &[Some("C")]),
                        ("CCCORACTIV", &[Some("1")]),
                        ("CCNOCLIIND", &[None]),
                    ]),
                );
            }
            "TRNSPACE" => {
                return Response::text(200, preview(&[("EDITFLAG", &[Some("X")])]));
            }
            "TADIR" => (world.directory, quoted.last().copied().unwrap_or_default()),
            "SEOCLASS" => (
                self.classes
                    .contains_key(quoted.first().copied().unwrap_or_default()),
                quoted.first().copied().unwrap_or_default(),
            ),
            _ => {
                let inactive = body.contains("AS4LOCAL = 'N'");

                (
                    if inactive {
                        world.inactive
                    } else {
                        world.definition
                    },
                    quoted.first().copied().unwrap_or_default(),
                )
            }
        };
        let columns: Vec<(&str, Vec<Option<&str>>)> = fields
            .iter()
            .map(|field| {
                let value = match *field {
                    "DEVCLASS" => "$TMP",
                    "PGMNA" => "ZREPORT",
                    _ => key,
                };

                (
                    *field,
                    if present {
                        vec![Some(value)]
                    } else {
                        Vec::new()
                    },
                )
            })
            .collect();
        let borrowed: Vec<(&str, &[Option<&str>])> = columns
            .iter()
            .map(|(field, values)| (*field, if values.is_empty() { none } else { &values[..] }))
            .collect();

        Response::text(200, preview(&borrowed))
    }
}

/// The class name of `/sap/bc/adt/oo/classes/<name>/source/main`.
fn name_of_source(path: &str) -> String {
    path.trim_start_matches("/sap/bc/adt/oo/classes/")
        .split('/')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase()
}

pub struct Session {
    sap: Shared,
    id: usize,
    admit: bool,
}

impl SadtTransport for Session {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        self.sap.borrow_mut().handle(self.id, request, self.admit)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.sap.borrow_mut().closes += 1;

        Ok(())
    }
}

pub type Opened = Result<Box<dyn SadtTransport>, TransportError>;

pub fn session(sap: &Shared, admit: bool) -> Session {
    let mut state = sap.borrow_mut();

    state.sessions += 1;

    Session {
        sap: sap.clone(),
        id: state.sessions,
        admit,
    }
}

pub fn http_session(sap: &Shared) -> Opened {
    Ok(Box::new(HttpTransport::wrapping(session(sap, false))))
}

pub fn rfc_session(sap: &Shared) -> Opened {
    Ok(Box::new(session(sap, true)))
}
