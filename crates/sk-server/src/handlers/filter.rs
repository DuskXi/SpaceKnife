//! filter.* (stateless: parsing, colouring, completion, vocabulary; Python
//! `server/filter_handlers.py`).

use sk_protocol::methods::{FilterParseMethod, FilterVocabMethod};

use crate::rpc::Router;
use crate::topics::TopicRegistry;
use crate::views::current_vocab;

pub fn install(r: &mut Router, _t: &mut TopicRegistry) {
    r.add::<FilterParseMethod, _, _>(
        |_, p| async move { Ok(current_vocab().parse(&p.text, p.cursor)) },
    );
    r.add::<FilterVocabMethod, _, _>(|_, _| async move { Ok(current_vocab().vocab().clone()) });
}
