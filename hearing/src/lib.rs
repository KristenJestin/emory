//! Les oreilles d'Emory, version 1 : l'oreille à bouton (`specs/oreilles.md`).
//!
//! Du son du poste (`poste.<nom>.audio`) aux phrases (`oreilles.phrase`) et au tour entier
//! (`oreilles.tour`), par Silero VAD et Parakeet.

pub mod audio;
pub mod bus;
pub mod config;
pub mod corrections;
pub mod moteur;
pub mod service;
pub mod tours;
