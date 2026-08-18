// autoreview: review the current repo's open PRs headlessly -- no terminal
// tabs. Each PR is reviewed by a `dash-p` subprocess driving claude; the run
// shows live per-PR progress, prints a summary, and exits nonzero if any
// review failed.
//
// This is the sibling of `review-prs` (bash, in this repo), which fans the
// same PRs out into one terminal tab each. The two agree on what is worth
// reviewing and on which session a PR belongs to: the selection and session
// derivation in src/ mirror lib/*.sh byte for byte, and the golden unit tests
// pin the session ids to lib/session.sh's output.

fn main() {
    eprintln!("autoreview: not implemented yet");
    std::process::exit(2);
}
