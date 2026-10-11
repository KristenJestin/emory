//! Les sujets du bus. Le nom de l'appareil y entre tel quel ; un programme qui écoute tous les
//! appareils s'abonne au sujet avec `*` et retrouve l'appareil par [`appareil_du_sujet`].

use crate::{Appareil, appareil::morceau_valide};

/// Les phrases transcrites pendant que Kris parle ([`crate::Phrase`]).
pub const PHRASE: &str = "oreilles.phrase";
/// Le tour entier, transcrit au second appui ([`crate::Tour`]).
pub const TOUR: &str = "oreilles.tour";
/// Une phrase à dire ([`crate::Dire`]).
pub const DIRE: &str = "bouche.dire";
/// Tout ce qui n'est pas encore dit d'un tour est jeté ([`crate::Stop`]).
pub const STOP: &str = "bouche.stop";
/// L'esprit demande à tous les postes et toutes les mains de s'annoncer (message vide).
pub const ANNONCES_DEMANDE: &str = "annonces.demande";

/// Les appuis de tous les postes.
pub const TOUS_LES_BOUTONS: &str = "poste.*.bouton";
/// Le son de tous les postes.
pub const TOUS_LES_SONS: &str = "poste.*.audio";
/// Les annonces de tous les postes.
pub const TOUTES_LES_ANNONCES_DE_POSTE: &str = "poste.*.annonce";
/// Les annonces de toutes les mains.
pub const TOUTES_LES_ANNONCES_DE_MAIN: &str = "main.*.annonce";

/// Les appuis d'un poste ([`crate::Bouton`]).
pub fn bouton(a: &Appareil) -> String {
    format!("poste.{a}.bouton")
}

/// Le son du micro d'un poste (son brut, voir [`crate::son`]).
pub fn son(a: &Appareil) -> String {
    format!("poste.{a}.audio")
}

/// La voix d'Emory, de la bouche au poste (son brut, voir [`crate::son`]).
pub fn son_retour(a: &Appareil) -> String {
    format!("poste.{a}.audio.retour")
}

/// L'annonce d'un poste ([`crate::Annonce`]).
pub fn annonce_poste(a: &Appareil) -> String {
    format!("poste.{a}.annonce")
}

/// L'annonce d'une main ([`crate::Annonce`]).
pub fn annonce_main(a: &Appareil) -> String {
    format!("main.{a}.annonce")
}

/// La bouche commence ou finit de parler sur un appareil ([`crate::Parle`]).
pub fn parle(a: &Appareil) -> String {
    format!("bouche.{a}.parle")
}

/// Toutes les actions d'une main, pour s'y abonner.
pub fn actions(a: &Appareil) -> String {
    format!("main.{a}.>")
}

/// Une action d'une main (`main.bureau.mpris.pause`), demande avec réponse
/// ([`crate::ReponseAction`]). `None` si le nom de l'action ne peut pas entrer dans un sujet.
pub fn action(a: &Appareil, nom: &str) -> Option<String> {
    (nom != "annonce" && nom.split('.').all(morceau_valide)).then(|| format!("main.{a}.{nom}"))
}

/// L'appareil d'un sujet `poste.<nom>.…`, `main.<nom>.…` ou `bouche.<nom>.…`.
pub fn appareil_du_sujet(sujet: &str) -> Option<Appareil> {
    let mut morceaux = sujet.split('.');
    match morceaux.next()? {
        "poste" | "main" | "bouche" => Appareil::new(morceaux.next()?).ok(),
        _ => None,
    }
}

/// Le nom de l'action d'un sujet `main.<nom>.<action>` (`mpris.pause`).
pub fn action_du_sujet(sujet: &str) -> Option<&str> {
    let reste = sujet.strip_prefix("main.")?;
    let (_, action) = reste.split_once('.')?;
    (action != "annonce").then_some(action)
}
