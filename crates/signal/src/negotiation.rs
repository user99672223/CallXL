//! Perfect negotiation (W3C pattern) for glare handling (§4).
//! The polite peer is the one with the lexicographically smaller signing key.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfferAction {
    /// Apply the remote offer and answer it.
    Accept,
    /// Impolite side during glare: drop the remote offer, ours wins.
    Ignore,
    /// Polite side during glare: roll back our offer, then accept theirs.
    RollbackThenAccept,
}

#[derive(Clone, Debug)]
pub struct Negotiator {
    pub polite: bool,
    pub making_offer: bool,
    pub ignore_offer: bool,
}

pub fn is_polite(local_key: &str, remote_key: &str) -> bool {
    local_key < remote_key
}

impl Negotiator {
    pub fn new(local_key: &str, remote_key: &str) -> Self {
        Self { polite: is_polite(local_key, remote_key), making_offer: false, ignore_offer: false }
    }

    /// Decide how to handle an incoming offer. `stable` = signaling state is stable.
    pub fn on_remote_offer(&mut self, stable: bool) -> OfferAction {
        let collision = self.making_offer || !stable;
        self.ignore_offer = !self.polite && collision;
        if self.ignore_offer {
            OfferAction::Ignore
        } else if collision {
            OfferAction::RollbackThenAccept
        } else {
            OfferAction::Accept
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glare_resolves_to_one_offer() {
        let mut a = Negotiator::new("aa", "bb"); // polite
        let mut b = Negotiator::new("bb", "aa"); // impolite
        a.making_offer = true;
        b.making_offer = true;
        assert_eq!(a.on_remote_offer(false), OfferAction::RollbackThenAccept);
        assert_eq!(b.on_remote_offer(false), OfferAction::Ignore);
        let mut c = Negotiator::new("bb", "aa");
        assert_eq!(c.on_remote_offer(true), OfferAction::Accept);
    }
}
