use std::{
    borrow::Cow,
    fs::{self, File},
    io::{self, BufRead, BufReader, BufWriter, Error, ErrorKind, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

const WRITER_BUFFER_BYTES: usize = 64 * 1024;
const READER_BUFFER_BYTES: usize = 64 * 1024;

/// A single delivered impression and its observed click outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpressionClickRecord {
    pub day: usize,
    pub user_id: String,
    pub organic_item_id: String,
    pub organic_category_id: String,
    pub campaign_id: String,
    pub advertiser_id: String,
    pub ad_item_id: String,
    pub ad_category_id: String,
    pub ad_price_cents: u32,
    pub cpc_cents: u64,
    pub clicked: bool,
}

/// Borrowed view of an impression-click record for allocation-free serialization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ImpressionClickRef<'a> {
    pub day: usize,
    pub user_id: &'a str,
    pub organic_item_id: &'a str,
    pub organic_category_id: &'a str,
    pub campaign_id: &'a str,
    pub advertiser_id: &'a str,
    pub ad_item_id: &'a str,
    pub ad_category_id: &'a str,
    pub ad_price_cents: u32,
    pub cpc_cents: u64,
    pub clicked: bool,
}

/// Zero-copy-friendly deserialized view of a JSONL impression-click record.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct BorrowedImpressionClickRecord<'a> {
    pub day: usize,
    #[serde(borrow)]
    pub user_id: Cow<'a, str>,
    #[serde(borrow)]
    pub organic_item_id: Cow<'a, str>,
    #[serde(borrow)]
    pub organic_category_id: Cow<'a, str>,
    #[serde(borrow)]
    pub campaign_id: Cow<'a, str>,
    #[serde(borrow)]
    pub advertiser_id: Cow<'a, str>,
    #[serde(borrow)]
    pub ad_item_id: Cow<'a, str>,
    #[serde(borrow)]
    pub ad_category_id: Cow<'a, str>,
    pub ad_price_cents: u32,
    pub cpc_cents: u64,
    pub clicked: bool,
}

impl BorrowedImpressionClickRecord<'_> {
    pub fn into_owned(self) -> ImpressionClickRecord {
        ImpressionClickRecord {
            day: self.day,
            user_id: self.user_id.into_owned(),
            organic_item_id: self.organic_item_id.into_owned(),
            organic_category_id: self.organic_category_id.into_owned(),
            campaign_id: self.campaign_id.into_owned(),
            advertiser_id: self.advertiser_id.into_owned(),
            ad_item_id: self.ad_item_id.into_owned(),
            ad_category_id: self.ad_category_id.into_owned(),
            ad_price_cents: self.ad_price_cents,
            cpc_cents: self.cpc_cents,
            clicked: self.clicked,
        }
    }
}

/// Observable request-time context for ranking candidate ads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RankingContext<'a> {
    pub user_id: &'a str,
    pub organic_category_id: &'a str,
}

/// Observable metadata for an eligible ad candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdCandidate<'a> {
    pub campaign_id: &'a str,
    pub advertiser_id: &'a str,
    pub ad_item_id: &'a str,
    pub ad_category_id: &'a str,
    pub ad_price_cents: u32,
}

/// Buffered JSONL writer for one day's impression-click log.
pub struct DailyImpressionWriter {
    day: usize,
    path: PathBuf,
    writer: BufWriter<File>,
    records_written: u64,
}

impl DailyImpressionWriter {
    pub fn day(&self) -> usize {
        self.day
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn records_written(&self) -> u64 {
        self.records_written
    }

    pub fn write(&mut self, record: &ImpressionClickRecord) -> io::Result<()> {
        serde_json::to_writer(&mut self.writer, record).map_err(json_to_io_error)?;
        self.writer.write_all(b"\n")?;
        self.records_written += 1;
        Ok(())
    }

    pub fn write_ref(&mut self, record: &ImpressionClickRef<'_>) -> io::Result<()> {
        serde_json::to_writer(&mut self.writer, record).map_err(json_to_io_error)?;
        self.writer.write_all(b"\n")?;
        self.records_written += 1;
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<u64> {
        self.writer.flush()?;
        Ok(self.records_written)
    }
}

/// On-disk store organizing impression-click logs into one JSONL file per day (`day-01.jsonl`, ...).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImpressionLogStore {
    root: PathBuf,
}

impl ImpressionLogStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn day_file_path(&self, day: usize) -> PathBuf {
        self.root.join(format!("day-{day:02}.jsonl"))
    }

    pub fn model_file_path(&self, day: usize) -> PathBuf {
        self.root.join(format!("model-day-{day:02}.json"))
    }

    /// Ensure the log directory exists and remove any previous `day-*.jsonl` or `model-day-*.json` files.
    pub fn clear(&self) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|name| name.to_str())
                && (parse_day_filename(name).is_some() || parse_model_filename(name).is_some())
            {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    /// Create (or overwrite) the daily JSONL log file for `day`.
    pub fn create_day_writer(&self, day: usize) -> io::Result<DailyImpressionWriter> {
        if day == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "day must be 1 or greater",
            ));
        }
        fs::create_dir_all(&self.root)?;
        let path = self.day_file_path(day);
        let file = File::create(&path)?;
        Ok(DailyImpressionWriter {
            day,
            path,
            writer: BufWriter::with_capacity(WRITER_BUFFER_BYTES, file),
            records_written: 0,
        })
    }

    /// Return all sorted day numbers currently present on disk.
    pub fn available_days(&self) -> io::Result<Vec<usize>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut days = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if let Some(day) = entry
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(parse_day_filename)
            {
                days.push(day);
            }
        }
        days.sort_unstable();
        Ok(days)
    }

    /// Stream every record in a single day's JSONL file with minimal allocations.
    pub fn for_each_record_in_day(
        &self,
        day: usize,
        mut visit: impl FnMut(&BorrowedImpressionClickRecord<'_>),
    ) -> io::Result<()> {
        if day == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "day must be 1 or greater",
            ));
        }
        let path = self.day_file_path(day);
        let file = File::open(&path)?;
        let mut reader = BufReader::with_capacity(READER_BUFFER_BYTES, file);
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let record: BorrowedImpressionClickRecord<'_> =
                serde_json::from_str(trimmed).map_err(json_to_io_error)?;
            visit(&record);
        }
        Ok(())
    }

    /// Stream every record across days `1..=through_day` in chronological order.
    pub fn for_each_record_through_day(
        &self,
        through_day: usize,
        mut visit: impl FnMut(&BorrowedImpressionClickRecord<'_>),
    ) -> io::Result<()> {
        for day in 1..=through_day {
            self.for_each_record_in_day(day, &mut visit)?;
        }
        Ok(())
    }

    /// Load all owned records from a single day's JSONL file.
    pub fn read_day(&self, day: usize) -> io::Result<Vec<ImpressionClickRecord>> {
        let mut records = Vec::new();
        self.for_each_record_in_day(day, |record| {
            records.push(record.clone().into_owned());
        })?;
        Ok(records)
    }

    /// Load all owned records from days `1..=through_day` in chronological order.
    pub fn read_through_day(&self, through_day: usize) -> io::Result<Vec<ImpressionClickRecord>> {
        let mut records = Vec::new();
        self.for_each_record_through_day(through_day, |record| {
            records.push(record.clone().into_owned());
        })?;
        Ok(records)
    }
}

fn parse_day_filename(name: &str) -> Option<usize> {
    let digits = name.strip_prefix("day-")?.strip_suffix(".jsonl")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<usize>().ok().filter(|&day| day > 0)
}

fn parse_model_filename(name: &str) -> Option<usize> {
    let digits = name.strip_prefix("model-day-")?.strip_suffix(".json")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<usize>().ok().filter(|&day| day > 0)
}

fn json_to_io_error(error: serde_json::Error) -> Error {
    Error::new(ErrorKind::InvalidData, error)
}
