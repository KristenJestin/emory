//! Le son des messages du poste : les en-têtes et les échantillons.

use std::str::FromStr;

use anyhow::{Context, Result, bail};
use contract::{IdTour, son};

/// Ce que disent les en-têtes d'un morceau de son du poste.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnteteSon {
    pub tour: IdTour,
    pub seq: u32,
}

/// Lit les en-têtes d'un message de son et vérifie que le format est celui du micro
/// ([`son::MICRO`]).
pub fn lire_entetes(entetes: Option<&async_nats::HeaderMap>) -> Result<EnteteSon> {
    let entetes = entetes.context("message de son sans en-têtes")?;
    let valeur = |nom: &str| {
        entetes
            .get(nom)
            .map(|v| v.as_str())
            .with_context(|| format!("en-tête {nom} absent"))
    };
    let format = son::Format::from_str(valeur(son::entetes::FORMAT)?)?;
    if format != son::MICRO {
        bail!("format {format} au lieu de {}", son::MICRO);
    }
    Ok(EnteteSon {
        tour: valeur(son::entetes::TOUR)?
            .parse()
            .context("en-tête Emory-Tour")?,
        seq: valeur(son::entetes::SEQ)?
            .parse()
            .context("en-tête Emory-Seq")?,
    })
}

/// Les échantillons d'un morceau de PCM 16 bits petit-boutiste, entre -1 et 1.
pub fn echantillons(octets: &[u8]) -> Result<Vec<f32>> {
    if !octets.len().is_multiple_of(2) {
        bail!(
            "{} octets : pas un nombre entier d'échantillons",
            octets.len()
        );
    }
    Ok(octets
        .as_chunks::<2>()
        .0
        .iter()
        .map(|o| i16::from_le_bytes(*o) as f32 / 32768.0)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entetes(format: &str, seq: &str) -> async_nats::HeaderMap {
        let mut h = async_nats::HeaderMap::new();
        h.insert(son::entetes::TOUR, IdTour::nouveau().to_string());
        h.insert(son::entetes::SEQ, seq);
        h.insert(son::entetes::FORMAT, format);
        h
    }

    #[test]
    fn les_entetes_du_micro_sont_lus() {
        let lus = lire_entetes(Some(&entetes(&son::MICRO.to_string(), "7"))).unwrap();
        assert_eq!(lus.seq, 7);
    }

    #[test]
    fn un_autre_format_un_entete_absent_ou_faux_sont_refuses() {
        assert!(lire_entetes(Some(&entetes("s16le;24000;1", "0"))).is_err());
        assert!(lire_entetes(Some(&entetes("n'importe quoi", "0"))).is_err());
        assert!(lire_entetes(Some(&entetes("s16le;16000;1", "x"))).is_err());
        assert!(lire_entetes(Some(&async_nats::HeaderMap::new())).is_err());
        assert!(lire_entetes(None).is_err());
    }

    #[test]
    fn les_echantillons_sont_decodes_en_petit_boutiste() {
        let octets = [0x00, 0x00, 0x00, 0x40, 0x00, 0x80, 0xff, 0x7f];
        let e = echantillons(&octets).unwrap();
        assert_eq!(e[0], 0.0);
        assert_eq!(e[1], 0.5);
        assert_eq!(e[2], -1.0);
        assert!((e[3] - 0.99997).abs() < 1e-4);
        assert!(echantillons(&[0, 0, 0]).is_err());
        assert!(echantillons(&[]).unwrap().is_empty());
    }
}
