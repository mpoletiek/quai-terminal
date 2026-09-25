//! Saving a Quai account or a payment code to a contact that already exists.
//!
//! A contact is one person: one payment code, and as many Quai accounts as they use. The
//! contact's `address` is the account payments go to by name; the others are kept beside it
//! (`contact_addresses`), so saving another account never loses one. A payment code, and an
//! account, belongs to one contact at a time: saving it to someone else moves it.
//!
//! Anything that would replace or move what is saved is said first ([`plan`]), for the user to
//! confirm; [`Session::save_to_contact`] then does exactly that.

use crate::appdb::Contact;
use crate::error::{CoreError, Result};
use crate::registry::parse_any_address;
use crate::session::{Session, short_address, short_code};
use quai_sdk::payments::PaymentCode;
use serde::{Deserialize, Serialize};

/// What is saved to a contact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ContactValue {
    /// A Quai account, lowercase.
    Account(String),
    /// A payment code, exactly as given (base58 is case-sensitive).
    PaymentCode(String),
}

impl ContactValue {
    /// A Quai account (`0x…`) or a payment code (`PM8T…`). A Qi address is neither: it is the
    /// contact's own address, set with an edit.
    pub fn parse(text: &str) -> Result<ContactValue> {
        let text = text.trim();
        if text.starts_with("0x") {
            let parsed = parse_any_address(text).map_err(|_| CoreError::Invalid(format!("`{text}` is not a valid address")))?;
            if parsed.ledger() == quai_sdk::primitives::Ledger::Qi {
                return Err(CoreError::Invalid(
                    "that is a Qi address: it goes in the contact's address (`contact edit NAME --address`)".into(),
                ));
            }
            return Ok(ContactValue::Account(text.to_lowercase()));
        }
        PaymentCode::from_base58(text).map_err(|_| CoreError::Invalid(format!("`{text}` is neither a Quai account nor a payment code")))?;
        Ok(ContactValue::PaymentCode(text.to_string()))
    }

    /// How it reads in a sentence.
    pub fn describe(&self) -> String {
        match self {
            ContactValue::Account(a) => format!("account {}", short_address(a)),
            ContactValue::PaymentCode(c) => format!("payment code {}", short_code(c)),
        }
    }
}

/// What saving a value to a contact will do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactSave {
    /// The contact's name.
    pub contact: String,
    pub value: ContactValue,
    /// What it replaces or moves, one sentence each. Empty: nothing saved is lost or moved.
    pub warnings: Vec<String>,
    /// The contact already has it: nothing to do.
    pub unchanged: bool,
}

/// Plan saving `value` to the contact named `target`, from the contacts as they stand.
/// `accounts` is every (address, contact name) saved beside a contact's own address; `mine` is
/// this wallet's own accounts and `own_code` its payment code, which are never saved to anyone.
pub fn plan(
    contacts: &[Contact],
    accounts: &[(String, String)],
    mine: &[String],
    own_code: Option<&str>,
    target: &str,
    value: ContactValue,
) -> Result<ContactSave> {
    let contact = contacts
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(target))
        .ok_or_else(|| CoreError::NotFound(format!("no contact `{target}`")))?;
    let name = contact.name.clone();
    let mut warnings = Vec::new();
    let mut unchanged = false;
    // Whoever else holds it, and whether taking it leaves them with nothing.
    let emptied = |other: &Contact, without: &dyn Fn(&str) -> bool| {
        let others: Vec<&String> = accounts.iter().filter(|(_, n)| *n == other.name).map(|(a, _)| a).collect();
        let address_left = other.address.as_deref().is_some_and(|a| !without(a)) || others.iter().any(|a| !without(a));
        !address_left
    };
    match &value {
        ContactValue::PaymentCode(code) => {
            if own_code == Some(code.as_str()) {
                return Err(CoreError::Invalid("that is your own payment code".into()));
            }
            match contact.payment_code.as_deref() {
                Some(c) if c == code => unchanged = true,
                Some(old) => warnings.push(format!(
                    "replaces {name}'s payment code {}: private payments go to the new one; Qi already received on the old one stays yours",
                    short_code(old)
                )),
                None => {}
            }
            for other in contacts.iter().filter(|c| c.id != contact.id && c.payment_code.as_deref() == Some(code.as_str())) {
                let gone = emptied(other, &|_| false) && other.address.is_none();
                warnings.push(format!(
                    "moves it from {} to {name}: a payment code is one person{}",
                    other.name,
                    if gone { format!(", and {} is removed: it has nothing else saved", other.name) } else { String::new() }
                ));
            }
        }
        ContactValue::Account(account) => {
            if mine.iter().any(|m| m.eq_ignore_ascii_case(account)) {
                return Err(CoreError::Invalid("that is one of this wallet's own accounts".into()));
            }
            let holds = |c: &Contact| {
                c.address.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(account))
                    || accounts.iter().any(|(a, n)| *n == c.name && a.eq_ignore_ascii_case(account))
            };
            unchanged = holds(contact);
            for other in contacts.iter().filter(|c| c.id != contact.id && holds(c)) {
                let gone = other.payment_code.is_none() && emptied(other, &|a| a.eq_ignore_ascii_case(account));
                warnings.push(format!(
                    "moves it from {} to {name}{}",
                    other.name,
                    if gone { format!(", and {} is removed: it has nothing else saved", other.name) } else { String::new() }
                ));
            }
        }
    }
    let unchanged = unchanged && warnings.is_empty();
    Ok(ContactSave { contact: name, value, warnings, unchanged })
}

impl Session {
    /// Every account saved beside a contact's own address, as (address, contact name).
    pub fn contact_accounts(&self) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        for c in self.app.contacts()? {
            out.extend(self.app.contact_addresses(c.id)?.into_iter().map(|a| (a, c.name.clone())));
        }
        Ok(out)
    }

    /// What saving `value` (a Quai account or a payment code) to `contact` would replace or move.
    pub fn plan_contact_save(&self, contact: &str, value: &str) -> Result<ContactSave> {
        plan(
            &self.app.contacts()?,
            &self.contact_accounts()?,
            &self.meta.quai_owner_addresses(),
            self.meta.payment_code.as_deref(),
            contact,
            ContactValue::parse(value)?,
        )
    }

    /// Save a Quai account or a payment code to an existing contact, replacing or moving what
    /// [`Self::plan_contact_save`] said (the caller has confirmed it). An account is added beside
    /// the others, and becomes the contact's address when it has none; a payment code replaces
    /// the one saved. Taken from another contact, it leaves them; one left with nothing is removed.
    pub fn save_to_contact(&mut self, contact: &str, value: &str) -> Result<(Contact, ContactSave)> {
        let save = self.plan_contact_save(contact, value)?;
        let contacts = self.app.contacts()?;
        let target = contacts.iter().find(|c| c.name == save.contact).cloned().ok_or_else(|| CoreError::NotFound(contact.into()))?;
        match &save.value {
            ContactValue::PaymentCode(code) => {
                for mut other in contacts.into_iter().filter(|c| c.id != target.id && c.payment_code.as_deref() == Some(code.as_str())) {
                    other.payment_code = None;
                    self.keep_or_remove(other)?;
                }
                let mut target = target;
                target.payment_code = Some(code.clone());
                self.app.update_contact(&target)?;
                if self.is_unlocked() {
                    self.ensure_channel(&PaymentCode::from_base58(code)?)?;
                }
            }
            ContactValue::Account(account) => {
                for mut other in contacts.into_iter().filter(|c| c.id != target.id) {
                    let listed = self.app.remove_contact_address(other.id, account)?;
                    let own = other.address.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(account));
                    if own {
                        // The next account they are known by takes its place.
                        other.address = self.app.contact_addresses(other.id)?.into_iter().next();
                    }
                    if listed || own {
                        self.keep_or_remove(other)?;
                    }
                }
                self.app.add_contact_address(target.id, account)?;
                if target.address.is_none() {
                    let mut target = target;
                    target.address = Some(account.clone());
                    self.app.update_contact(&target)?;
                }
            }
        }
        let saved = self.app.contact(&save.contact)?.ok_or_else(|| CoreError::Storage("contact vanished after saving".into()))?;
        Ok((saved, save))
    }

    /// Forget one of a contact's accounts. Its own address passes to the next one; the last thing
    /// a contact holds cannot go (remove the contact instead).
    pub fn forget_contact_account(&mut self, contact: &str, account: &str) -> Result<Contact> {
        let mut c = self.app.contact(contact)?.ok_or_else(|| CoreError::NotFound(format!("no contact `{contact}`")))?;
        let account = account.trim().to_lowercase();
        let listed = self.app.contact_addresses(c.id)?;
        let own = c.address.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(&account));
        if !own && !listed.contains(&account) {
            return Err(CoreError::NotFound(format!("{} has no account {}", c.name, short_address(&account))));
        }
        let rest: Vec<String> = listed.into_iter().filter(|a| *a != account).collect();
        let next = if own { rest.first().cloned() } else { c.address.clone() };
        if next.is_none() && c.payment_code.is_none() {
            return Err(CoreError::Invalid(format!("that is all {} has saved: remove the contact instead", c.name)));
        }
        self.app.remove_contact_address(c.id, &account)?;
        c.address = next;
        self.app.update_contact(&c)?;
        Ok(c)
    }

    /// A contact that still holds something is kept as it now is; one left with nothing goes.
    fn keep_or_remove(&self, contact: Contact) -> Result<()> {
        if contact.address.is_none() && contact.payment_code.is_none() && self.app.contact_addresses(contact.id)?.is_empty() {
            self.app.remove_contact(&contact.name)?;
        } else {
            self.app.update_contact(&contact)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE_A: &str =
        "PM8TJbuTpdxoPzWYyXBe3v28HwBi9sdhRMxBg1wwfzHhCDKHvV2HbM5YYYejtZsDKMPqh7ygrhQ8Ygdbovx38QQcmPTKVhtyozcedXn8BfRWY7dgnfn7";
    const CODE_B: &str =
        "PM8TJbmQ2ocBeDo8FnRPq3M2uzG82zZC2UcMwtJFsWtcuzwjppmXD6nLtGAF7RK1U8heYjadxrh6dcX2Zfp9QV17m9BWkYcMdWyuoS4C5QbgmjYPDGG9";
    const ONE: &str = "0x004dd9afaa2768642b5cde15c24f37bf19d842e4";
    const TWO: &str = "0x002162a69c50b31bf09a2727ab89a1a9852ccefe";

    fn contact(id: i64, name: &str, address: Option<&str>, code: Option<&str>) -> Contact {
        Contact { id, name: name.into(), address: address.map(Into::into), payment_code: code.map(Into::into), note: String::new() }
    }

    fn run(contacts: &[Contact], accounts: &[(&str, &str)], target: &str, value: &str) -> Result<ContactSave> {
        let accounts: Vec<(String, String)> = accounts.iter().map(|(a, n)| (a.to_string(), n.to_string())).collect();
        plan(contacts, &accounts, &["0x00aa000000000000000000000000000000000001".into()], Some(CODE_B), target, ContactValue::parse(value)?)
    }

    #[test]
    fn adding_is_quiet_and_replacing_is_said() {
        let bob = contact(1, "Bob", Some(ONE), None);
        let s = run(std::slice::from_ref(&bob), &[(ONE, "Bob")], "bob", TWO).unwrap();
        assert!(s.warnings.is_empty() && !s.unchanged, "a second account is added, not a replacement: {s:?}");
        assert!(run(std::slice::from_ref(&bob), &[(ONE, "Bob")], "Bob", ONE).unwrap().unchanged);
        let s = run(std::slice::from_ref(&bob), &[], "Bob", CODE_A).unwrap();
        assert!(s.warnings.is_empty(), "a first payment code replaces nothing");

        let coded = contact(1, "Bob", Some(ONE), Some("PM8TJold"));
        let s = run(&[coded], &[], "Bob", CODE_A).unwrap();
        assert_eq!(s.warnings.len(), 1);
        assert!(s.warnings[0].starts_with("replaces Bob's payment code"), "{:?}", s.warnings);
    }

    #[test]
    fn taking_from_another_contact_is_a_move_and_says_who_is_left_empty() {
        let contacts = [contact(1, "Bob", None, Some(CODE_A)), contact(2, "Old Bob", Some(TWO), None)];
        let s = run(&contacts, &[(TWO, "Old Bob")], "Bob", TWO).unwrap();
        assert_eq!(s.warnings, vec!["moves it from Old Bob to Bob, and Old Bob is removed: it has nothing else saved".to_string()]);
        let contacts = [contact(1, "Bob", None, Some(CODE_A)), contact(2, "Old Bob", Some(TWO), Some("PM8TJx"))];
        let s = run(&contacts, &[], "Bob", TWO).unwrap();
        assert_eq!(s.warnings, vec!["moves it from Old Bob to Bob".to_string()], "kept: it still has a code");

        let contacts = [contact(1, "Alice", Some(ONE), None), contact(2, "Bob", None, Some(CODE_A))];
        let s = run(&contacts, &[], "Alice", CODE_A).unwrap();
        assert!(
            s.warnings[0].starts_with("moves it from Bob to Alice: a payment code is one person, and Bob is removed"),
            "{:?}",
            s.warnings
        );
    }

    /// Applied: an account moves between contacts (its old owner's next account takes its place,
    /// and a contact left with nothing goes), and forgetting keeps the last thing a contact holds.
    #[test]
    fn saves_move_accounts_and_codes_and_forget_keeps_the_last() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
        let registry = crate::registry::Registry::fast(paths);
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let meta = registry.create_hd("main", phrase, "english", "", "password123", true).unwrap();
        let network = crate::network::NetworkProfile::builtins().into_iter().next().unwrap();
        let mut s = Session::open(registry, crate::config::AppConfig::default(), meta, network).unwrap();
        let three = "0x0031313131313131313131313131313131313131";
        s.app.add_contact("Bob", Some(ONE), None, "").unwrap();
        let alias = s.app.add_contact("Robert", Some(TWO), None, "").unwrap();
        s.app.add_contact_address(alias, TWO).unwrap();
        s.app.add_contact_address(alias, three).unwrap();

        let (bob, _) = s.save_to_contact("bob", TWO).unwrap();
        assert_eq!(bob.address.as_deref(), Some(ONE), "an added account leaves where payments go");
        let robert = s.app.contact("Robert").unwrap().unwrap();
        assert_eq!(robert.address.as_deref(), Some(three), "Robert's next account takes the moved one's place");
        let (_, _) = s.save_to_contact("Bob", three).unwrap();
        assert!(s.app.contact("Robert").unwrap().is_none(), "left with nothing, Robert goes");
        let mut known = s.app.contact_addresses(bob.id).unwrap();
        known.sort();
        assert_eq!(known, vec![TWO.to_string(), three.to_string()]);

        let (bob, save) = s.save_to_contact("Bob", CODE_A).unwrap();
        assert!(save.warnings.is_empty());
        assert_eq!(bob.payment_code.as_deref(), Some(CODE_A));
        let (bob, save) = s.save_to_contact("Bob", CODE_B).unwrap();
        assert_eq!(bob.payment_code.as_deref(), Some(CODE_B), "a new code replaces the old");
        assert!(save.warnings[0].starts_with("replaces Bob's payment code"));

        s.forget_contact_account("Bob", ONE).unwrap();
        let bob = s.app.contact("Bob").unwrap().unwrap();
        assert!(bob.address.as_deref().is_some_and(|a| a == TWO || a == three), "the next account is where payments go");
        s.forget_contact_account("Bob", TWO).unwrap();
        s.forget_contact_account("Bob", three).unwrap();
        let bob = s.app.contact("Bob").unwrap().unwrap();
        assert_eq!((bob.address, bob.payment_code.as_deref()), (None, Some(CODE_B)), "the code alone is still Bob");
        s.save_to_contact("Bob", ONE).unwrap();
        s.app.update_contact(&crate::appdb::Contact { payment_code: None, ..s.app.contact("Bob").unwrap().unwrap() }).unwrap();
        assert!(s.forget_contact_account("Bob", ONE).is_err(), "the last thing a contact holds stays");
    }

    #[test]
    fn your_own_and_the_wrong_kind_are_refused() {
        let bob = [contact(1, "Bob", Some(ONE), None)];
        assert!(run(&bob, &[], "Bob", CODE_B).is_err(), "your own payment code");
        assert!(run(&bob, &[], "Bob", "0x00aa000000000000000000000000000000000001").is_err(), "your own account");
        assert!(run(&bob, &[], "Carol", TWO).is_err(), "no such contact");
        assert!(ContactValue::parse("hello").is_err());
        assert!(matches!(ContactValue::parse(" 0x004DD9AFAA2768642B5CDE15C24F37BF19D842E4 "), Ok(ContactValue::Account(a)) if a == ONE));
    }
}
