//! Scan a .zrd file with the reader, reporting what it found.
use bridge_encodings::zrd::{Record, ZrdReader};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: scan_zrd <file.zrd>")?;
    let mut reader = ZrdReader::open(&path)?;
    println!("{path}: {} records", reader.len());

    let started = Instant::now();
    let (mut deals, mut unsolved, mut separators) = (0u64, 0u64, 0u64);
    for record in reader.records() {
        match record? {
            Record::Deal { table, .. } => {
                deals += 1;
                if table.is_none() {
                    unsolved += 1;
                }
            }
            Record::Separator => separators += 1,
        }
    }
    println!(
        "deals {deals}, unsolved {unsolved}, separators {separators} in {:.1}s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
