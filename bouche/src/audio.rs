//! Le son entre le moteur et le bus : l'en-tête WAV du flux du moteur, le découpage en morceaux
//! et les en-têtes NATS d'un morceau.

use std::time::Duration;

use async_nats::HeaderMap;
use contract::{IdTour, son::Format, son::entetes};

/// Ce que dit l'en-tête WAV du flux du moteur.
#[derive(Debug, PartialEq, Eq)]
pub enum WavHeader {
    /// Pas assez d'octets pour décider.
    Incomplete,
    /// Le format, et le nombre d'octets de l'en-tête (le son commence juste après).
    Ready { format: Format, data_offset: usize },
}

/// Lit l'en-tête d'un WAV PCM 16 bits, sans dépendre de sa longueur annoncée (le moteur diffuse :
/// la taille est fausse ou énorme).
pub fn parse_wav_header(bytes: &[u8]) -> Result<WavHeader, String> {
    if bytes.len() < 12 {
        return Ok(WavHeader::Incomplete);
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("le moteur n'a pas répondu un WAV".to_owned());
    }
    let mut format = None;
    let mut at = 12;
    loop {
        if bytes.len() < at + 8 {
            return Ok(WavHeader::Incomplete);
        }
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        if id == b"data" {
            let format = format.ok_or("WAV sans bloc fmt avant le son")?;
            return Ok(WavHeader::Ready {
                format,
                data_offset: at + 8,
            });
        }
        let body = at + 8;
        let end = body.saturating_add(size + size % 2);
        if id == b"fmt " {
            if bytes.len() < body + 16 {
                return Ok(WavHeader::Incomplete);
            }
            let field =
                |o: usize| u16::from_le_bytes(bytes[body + o..body + o + 2].try_into().unwrap());
            let (tag, canaux, bits) = (field(0), field(2), field(14));
            let frequence = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().unwrap());
            if tag != 1 || bits != 16 || canaux == 0 || frequence == 0 {
                return Err(format!(
                    "WAV non géré (format {tag}, {bits} bits, {canaux} canaux, {frequence} Hz)"
                ));
            }
            format = Some(Format { frequence, canaux });
        }
        at = end;
    }
}

/// Découpe le son qui arrive par paquets irréguliers en morceaux de taille fixe, en gardant le
/// dernier : le morceau qui finit une phrase doit porter `Emory-Fin`, et on ne sait qu'il est le
/// dernier qu'à la fin du flux.
///
/// Un morceau plein est rendu tout de suite s'il reste du son derrière lui (il n'est donc pas le
/// dernier) ; sinon il est retenu jusqu'au prochain paquet ou jusqu'à la fin. Le premier son part
/// ainsi sans attendre, sauf quand le flux tombe pile sur la taille d'un morceau.
pub struct Chunker {
    size: usize,
    buffer: Vec<u8>,
    held: Option<Vec<u8>>,
}

impl Chunker {
    /// `size` en octets, arrondie à un nombre pair d'échantillons entiers.
    pub fn new(size: usize, bytes_per_frame: usize) -> Self {
        let size = (size / bytes_per_frame).max(1) * bytes_per_frame;
        Self {
            size,
            buffer: Vec::new(),
            held: None,
        }
    }

    /// Les morceaux à envoyer maintenant (aucun n'est le dernier de la phrase).
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        while self.buffer.len() >= self.size {
            let chunk: Vec<u8> = self.buffer.drain(..self.size).collect();
            if let Some(previous) = self.held.replace(chunk) {
                out.push(previous);
            }
            if !self.buffer.is_empty() {
                out.extend(self.held.take());
            }
        }
        out
    }

    /// La fin du flux : les morceaux restants, le dernier est celui qui porte `Emory-Fin`. Vide si
    /// le moteur n'a rien rendu.
    pub fn finish(mut self) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = self.held.take().into_iter().collect();
        if !self.buffer.is_empty() {
            out.push(self.buffer);
        }
        out
    }
}

/// La durée du son de ce nombre d'octets.
pub fn duration_of(bytes: usize, format: Format) -> Duration {
    let frames = bytes as u64 / (2 * format.canaux as u64);
    Duration::from_micros(frames * 1_000_000 / format.frequence as u64)
}

/// Les en-têtes d'un morceau de la voix qui revient au poste.
pub fn chunk_headers(tour: IdTour, seq: u32, format: Format, phrase: u32, last: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(entetes::TOUR, tour.to_string());
    headers.insert(entetes::SEQ, seq.to_string());
    headers.insert(entetes::FORMAT, format.to_string());
    headers.insert(entetes::PHRASE, phrase.to_string());
    if last {
        headers.insert(entetes::FIN, "1");
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_header(frequence: u32, bits: u16) -> Vec<u8> {
        let mut h = b"RIFF".to_vec();
        h.extend(0xFFFF_FFFFu32.to_le_bytes());
        h.extend(b"WAVEfmt ");
        h.extend(16u32.to_le_bytes());
        h.extend(1u16.to_le_bytes());
        h.extend(1u16.to_le_bytes());
        h.extend(frequence.to_le_bytes());
        h.extend((frequence * 2).to_le_bytes());
        h.extend(2u16.to_le_bytes());
        h.extend(bits.to_le_bytes());
        h.extend(b"data");
        h.extend(1_000_000_000u32.to_le_bytes());
        h
    }

    #[test]
    fn reads_the_streaming_header_of_the_engine() {
        let h = wav_header(24_000, 16);
        assert_eq!(h.len(), 44);
        assert_eq!(
            parse_wav_header(&h),
            Ok(WavHeader::Ready {
                format: Format {
                    frequence: 24_000,
                    canaux: 1
                },
                data_offset: 44
            })
        );
    }

    #[test]
    fn a_short_header_is_incomplete_not_an_error() {
        let h = wav_header(24_000, 16);
        for n in [0, 11, 20, 35, 43] {
            assert_eq!(parse_wav_header(&h[..n]), Ok(WavHeader::Incomplete), "{n}");
        }
    }

    #[test]
    fn skips_unknown_blocks_and_refuses_what_it_cannot_play() {
        let mut h = wav_header(24_000, 16);
        let data = h.split_off(36);
        h.extend(b"LIST");
        h.extend(3u32.to_le_bytes());
        h.extend([1, 2, 3, 0]); // odd size: padded to even
        h.extend(data);
        assert!(matches!(
            parse_wav_header(&h),
            Ok(WavHeader::Ready {
                data_offset: 56,
                ..
            })
        ));
        assert!(parse_wav_header(&wav_header(24_000, 8)).is_err());
        assert!(parse_wav_header(b"<html>not a wav at all</html>").is_err());
    }

    fn feed(chunker: &mut Chunker, pieces: &[usize]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut n = 0u8;
        for len in pieces {
            let piece: Vec<u8> = (0..*len)
                .map(|_| {
                    n = n.wrapping_add(1);
                    n
                })
                .collect();
            out.extend(chunker.push(&piece));
        }
        out
    }

    #[test]
    fn chunks_have_a_fixed_size_and_nothing_is_lost() {
        let mut c = Chunker::new(10, 2);
        let mut all = feed(&mut c, &[7, 3, 25, 1, 9]); // 45 bytes
        all.extend(c.finish());
        assert!(all[..all.len() - 1].iter().all(|x| x.len() == 10));
        assert_eq!(all.last().unwrap().len(), 5);
        let flat: Vec<u8> = all.concat();
        assert_eq!(flat, (1..=45).collect::<Vec<u8>>());
    }

    #[test]
    fn the_last_chunk_is_held_back_when_the_stream_ends_on_a_boundary() {
        let mut c = Chunker::new(10, 2);
        let sent = feed(&mut c, &[10, 10]);
        assert_eq!(sent.len(), 1, "the second full chunk could be the last one");
        let rest = c.finish();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].len(), 10);
    }

    #[test]
    fn a_chunk_followed_by_more_sound_goes_out_at_once() {
        let mut c = Chunker::new(10, 2);
        assert_eq!(feed(&mut c, &[12]).len(), 1);
    }

    #[test]
    fn no_sound_gives_no_chunk() {
        assert!(Chunker::new(10, 2).finish().is_empty());
    }

    #[test]
    fn chunk_size_is_a_whole_number_of_samples() {
        let mut c = Chunker::new(2879, 2);
        let out = feed(&mut c, &[6000]);
        assert_eq!(out[0].len(), 2878);
    }

    #[test]
    fn durations() {
        let f = Format {
            frequence: 24_000,
            canaux: 1,
        };
        assert_eq!(duration_of(2880, f), Duration::from_millis(60));
    }

    #[test]
    fn headers_follow_the_contract() {
        let tour = IdTour::nouveau();
        let f = Format {
            frequence: 24_000,
            canaux: 1,
        };
        let h = chunk_headers(tour, 12, f, 3, false);
        assert_eq!(h.get(entetes::TOUR).unwrap().as_str(), tour.to_string());
        assert_eq!(h.get(entetes::SEQ).unwrap().as_str(), "12");
        assert_eq!(h.get(entetes::FORMAT).unwrap().as_str(), "s16le;24000;1");
        assert_eq!(h.get(entetes::PHRASE).unwrap().as_str(), "3");
        assert!(h.get(entetes::FIN).is_none());
        let h = chunk_headers(tour, 13, f, 3, true);
        assert_eq!(h.get(entetes::FIN).unwrap().as_str(), "1");
    }
}
