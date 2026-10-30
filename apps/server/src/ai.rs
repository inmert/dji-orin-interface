//! The services behind the portal's command bar: speech recognition for
//! hold-to-talk ([`voice`]) and the model that turns a sentence into a plan
//! ([`understand`]), each a set of providers behind one trait, with what
//! they share in [`online`] (the API keys, the HTTP helpers). The Orin only
//! ever validates and runs what comes back.

pub mod online;
pub mod understand;
pub mod voice;
