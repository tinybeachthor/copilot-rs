//! Driving an SMT solver as a subprocess.
//!
//! Over a pipe rather than through a C API, so there is no build-time
//! dependency on a solver and no linkage to keep working: a solver is something
//! the user has on `PATH`, or does not.

use crate::sexpr::{self, Sexpr};
use crate::{Error, Solver};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// What a solver said about a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// A model exists.
    Sat,
    /// No model exists.
    Unsat,
    /// The solver gave up.
    Unknown,
}

/// A running solver process.
pub struct Session {
    process: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// Everything sent, for reporting when a solver rejects a script.
    transcript: String,
}

impl Solver {
    /// The command that runs this solver in incremental SMT-LIB mode.
    fn command(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Solver::Z3 => ("z3", &["-in"]),
            Solver::Cvc5 => ("cvc5", &["--lang", "smt2", "--incremental"]),
        }
    }

    /// The name of the executable.
    pub fn program(self) -> &'static str {
        self.command().0
    }

    /// Whether this solver is on `PATH`.
    pub fn available(self) -> bool {
        Command::new(self.program())
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    /// The first available solver, preferring the order given.
    pub fn first_available(candidates: &[Solver]) -> Option<Solver> {
        candidates.iter().copied().find(|s| s.available())
    }
}

impl Session {
    /// Starts a solver.
    pub fn start(solver: Solver) -> Result<Self, Error> {
        let (program, arguments) = solver.command();
        let mut process = Command::new(program)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::SolverUnavailable {
                program,
                reason: e.to_string(),
            })?;

        let stdin = process.stdin.take().expect("stdin was piped");
        let stdout = BufReader::new(process.stdout.take().expect("stdout was piped"));
        Ok(Session {
            process,
            stdin,
            stdout,
            transcript: String::new(),
        })
    }

    /// Sends commands that produce no answer.
    pub fn send(&mut self, script: &str) -> Result<(), Error> {
        if script.is_empty() {
            return Ok(());
        }
        self.transcript.push_str(script);
        self.transcript.push('\n');
        writeln!(self.stdin, "{script}").map_err(Error::from)?;
        self.stdin.flush().map_err(Error::from)
    }

    /// Asks whether the assertions so far are satisfiable.
    pub fn check_sat(&mut self) -> Result<Answer, Error> {
        self.send("(check-sat)")?;
        let answer = self.read_sexpr()?;
        match answer.atom() {
            Some("sat") => Ok(Answer::Sat),
            Some("unsat") => Ok(Answer::Unsat),
            Some("unknown") => Ok(Answer::Unknown),
            _ => Err(self.rejected(&format!("{answer:?}"))),
        }
    }

    /// Reads the values of the given terms from the current model.
    ///
    /// Terms rather than variables, so an aggregate can be read one leaf at a
    /// time — `(select a #b...)` for an array element, `(Point-x p)` for a
    /// field. That avoids parsing the array and datatype literals the two
    /// solvers print quite differently.
    pub fn get_values(&mut self, terms: &[String]) -> Result<Vec<Sexpr>, Error> {
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        self.send(&format!("(get-value ({}))", terms.join(" ")))?;
        let answer = self.read_sexpr()?;
        let pairs = answer
            .list()
            .ok_or_else(|| self.rejected("get-value did not return a list"))?;
        if pairs.len() != terms.len() {
            return Err(self.rejected("get-value returned the wrong number of pairs"));
        }
        pairs
            .iter()
            .map(|pair| {
                pair.list()
                    .and_then(|p| p.get(1))
                    .cloned()
                    .ok_or_else(|| Error::Protocol("malformed get-value pair".into()))
            })
            .collect()
    }

    /// Opens a new assertion scope.
    pub fn push(&mut self) -> Result<(), Error> {
        self.send("(push 1)")
    }

    /// Discards the innermost assertion scope.
    pub fn pop(&mut self) -> Result<(), Error> {
        self.send("(pop 1)")
    }

    /// Reads one complete S-expression, or a bare token, from the solver.
    fn read_sexpr(&mut self) -> Result<Sexpr, Error> {
        let mut buffer = String::new();
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).map_err(Error::from)?;
            if read == 0 {
                return Err(Error::Protocol(
                    "the solver exited before answering".to_string(),
                ));
            }
            buffer.push_str(&line);

            // A solver may wrap a long answer across lines, so keep reading
            // until the parentheses balance.
            if balanced(&buffer) && !buffer.trim().is_empty() {
                return sexpr::parse(&buffer);
            }
        }
    }

    /// An error carrying enough of the script to see what the solver choked on.
    fn rejected(&self, detail: &str) -> Error {
        let tail: String = self
            .transcript
            .lines()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        Error::Protocol(format!("{detail}\nlast commands sent:\n{tail}"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "(exit)");
        let _ = self.stdin.flush();
        let _ = self.process.wait();
    }
}

fn balanced(text: &str) -> bool {
    let mut depth = 0i32;
    let mut in_quoted = false;
    for character in text.chars() {
        match character {
            '|' => in_quoted = !in_quoted,
            '(' if !in_quoted => depth += 1,
            ')' if !in_quoted => depth -= 1,
            _ => {}
        }
    }
    depth == 0
}
