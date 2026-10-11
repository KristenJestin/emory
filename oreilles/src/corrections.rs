//! La table de corrections, appliquée après la transcription (`specs/oreilles.md`).
//!
//! Un fichier TSV, une règle par ligne : `motif<TAB>remplacement`. Le motif est une expression
//! régulière (`a|b` pour plusieurs formes), cherchée sur des mots entiers, sans tenir compte de la
//! casse. Les lignes vides et celles qui commencent par `#` sont ignorées. Les règles passent dans
//! l'ordre du fichier : « chris net » doit précéder « chris ».
//!
//! Elle ne doit corriger que des formes qui ne sont pas des mots français : « trafic » ou
//! « chris » se font réécrire sinon.

use std::path::Path;

use anyhow::{Context, Result, bail};
use regex::{NoExpand, Regex};

#[derive(Debug)]
struct Regle {
    motif: Regex,
    remplacement: String,
}

#[derive(Debug, Default)]
pub struct Corrections {
    regles: Vec<Regle>,
}

impl Corrections {
    pub fn lire(chemin: &Path) -> Result<Self> {
        let texte = std::fs::read_to_string(chemin)
            .with_context(|| format!("lecture de {}", chemin.display()))?;
        Self::depuis_tsv(&texte).with_context(|| format!("table {}", chemin.display()))
    }

    pub fn depuis_tsv(texte: &str) -> Result<Self> {
        let mut regles = Vec::new();
        for (i, ligne) in texte.lines().enumerate() {
            if ligne.trim().is_empty() || ligne.starts_with('#') {
                continue;
            }
            let Some((motif, remplacement)) = ligne.split_once('\t') else {
                bail!(
                    "ligne {} : motif et remplacement séparés par une tabulation",
                    i + 1
                );
            };
            let motif = Regex::new(&format!(r"(?i)\b(?:{})\b", motif.trim()))
                .with_context(|| format!("ligne {} : motif invalide", i + 1))?;
            regles.push(Regle {
                motif,
                remplacement: remplacement.trim().to_owned(),
            });
        }
        Ok(Self { regles })
    }

    pub fn len(&self) -> usize {
        self.regles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.regles.is_empty()
    }

    pub fn appliquer(&self, texte: &str) -> String {
        let mut texte = texte.to_owned();
        for regle in &self.regles {
            texte = regle
                .motif
                .replace_all(&texte, NoExpand(&regle.remplacement))
                .into_owned();
        }
        texte
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Corrections {
        Corrections::depuis_tsv(
            "# motif\tremplacement\n\
             hippocamp|hypocamp\tHippocampe\n\
             \n\
             chris net\tkrisnet\n\
             chris\tKris\n\
             émery\tEmory\n",
        )
        .unwrap()
    }

    #[test]
    fn corrige_sans_tenir_compte_de_la_casse() {
        assert_eq!(
            table().appliquer("Hippocamp et HYPOCAMP"),
            "Hippocampe et Hippocampe"
        );
    }

    #[test]
    fn ne_corrige_que_des_mots_entiers() {
        let t = table();
        assert_eq!(t.appliquer("christophe"), "christophe");
        assert_eq!(t.appliquer("anti-chris, chris."), "anti-Kris, Kris.");
        assert_eq!(t.appliquer("hippocampes"), "hippocampes");
    }

    #[test]
    fn les_regles_passent_dans_l_ordre_du_fichier() {
        assert_eq!(
            table().appliquer("sur chris net et chris"),
            "sur krisnet et Kris"
        );
    }

    #[test]
    fn les_lettres_accentuees_sont_des_lettres() {
        let t = table();
        assert_eq!(t.appliquer("l'émery"), "l'Emory");
        assert_eq!(t.appliquer("rémery"), "rémery");
    }

    #[test]
    fn le_remplacement_est_pris_tel_quel() {
        let t = Corrections::depuis_tsv("prix\t$1 et \\0\n").unwrap();
        assert_eq!(t.appliquer("le prix"), "le $1 et \\0");
    }

    #[test]
    fn une_table_vide_ne_change_rien() {
        let t = Corrections::depuis_tsv("").unwrap();
        assert!(t.is_empty());
        assert_eq!(t.appliquer("rien à faire"), "rien à faire");
    }

    #[test]
    fn une_ligne_sans_tabulation_ou_un_motif_invalide_est_refuse() {
        assert!(Corrections::depuis_tsv("sans tabulation\n").is_err());
        assert!(Corrections::depuis_tsv("(ouvert\tx\n").is_err());
    }
}
