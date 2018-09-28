#![allow(dead_code)]

extern crate byteorder;
extern crate chrono;
extern crate ndarray;
extern crate num_traits;

use self::num_traits::cast::FromPrimitive;

use self::chrono::{DateTime, NaiveDateTime, Utc};

use std::fs::{File, OpenOptions};
use std::io::{Error, Read, Seek, SeekFrom, Write};

use self::byteorder::{ByteOrder, LittleEndian};

use std::collections::BTreeMap;

struct Header {
    file: File,
    
    data: BTreeMap<i64, u64>,
    last_timestamp: i64,
    last_timestamp_numb: i64,
    
    current_timestamp: i64,

    next_timestamp: i64,
    next_timestamp_pos: u64,
}

impl Header {
    fn open(name: &str) -> Result<Header, Error> {
        let mut file = open_and_check(name.to_owned() + ".h", 16)?;

        //read in the entire file
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
				let mut data = BTreeMap::new();

				let last_timestamp = if bytes.len() == 0 {
					let now = Utc::now().timestamp();
					data.insert(now as i64, 0);
					now
				} else {
					let mut numbers = vec![0u64; bytes.len() / 8];
					LittleEndian::read_u64_into(&bytes, numbers.as_mut_slice());

					for i in (0..numbers.len()).step_by(2) {
							data.insert(numbers[i] as i64, numbers[i + 1]);
					}
					numbers[numbers.len() - 2] as i64
				};

        println!("last_timestamp: {}", last_timestamp);
        Ok(Header {
            file: file,
            data: data,
            last_timestamp: last_timestamp as i64,
            last_timestamp_numb: last_timestamp / (u16::max_value() as i64),
            
            current_timestamp: 0,
            
            next_timestamp: 0,
            next_timestamp_pos: 0,
        })
    }
}

enum DecodeOptions { 
	Interleaved,
	Sequential(usize),
}

//rewrite using cont generics when availible
//https://github.com/rust-lang/rfcs/blob/master/text/2000-const-generics.md
pub struct Timeseries {
    data: File,
    header: Header, // add triple headers

    pub line_size: usize,
    time_buffer: [u8; 8],

    first_time_in_data: DateTime<Utc>,
    last_time_in_data: DateTime<Utc>,

    pub start_byte: u64,
    pub stop_byte: u64,
    
    decode_option: DecodeOptions,
}

// ---------------------------------------------------------------------
// -- we store only the last 4 bytes of the timestamp ------------------
// ---------------------------------------------------------------------

impl Timeseries {
    pub fn open(name: &str, line_size: usize) -> Result<Timeseries, Error> {
        let mut data = open_and_check(name.to_owned() + ".data", line_size)?;
        let header = Header::open(name)?;

        let first_time = Self::get_first_time_in_data(&mut data);
        let last_time = Self::get_last_time_in_data();

        Ok(Timeseries {
            data: data,
            header: header, // add triple headers

            line_size: line_size+2, //+2 accounts for u16 timestamp
            time_buffer: [0; 8],

            first_time_in_data: first_time,
            last_time_in_data: last_time,

            //these are set during: set_read_start, set_read_end then read is
            //bound by these points
            start_byte: 0,
            stop_byte: i64::max_value() as u64,
            
            decode_option: DecodeOptions::Sequential(8_000),
        })
    }

    fn get_first_time_in_data(data: &mut File) -> DateTime<Utc> {
        let mut buf = [0; 8]; //rewrite to use bufferd data
        if data.seek(SeekFrom::Start(0)).is_err() {
            data.read(&mut buf).unwrap();
            let timestamp = LittleEndian::read_u64(&buf) as i64;
            DateTime::<Utc>::from_utc(NaiveDateTime::from_timestamp(timestamp, 0), Utc)
        } else {
            warn!("file is empty");
            Utc::now()
        }
    }
    fn get_last_time_in_data() -> DateTime<Utc> {
        //let mut buf = [0; 8]; //rewrite to use bufferd data
        //let last_timestamp = if !file.seek(SeekFrom::End(-16)).is_err(){;
        //file.read(&mut buf);
        //LittleEndian::read_u64(&buf) as i64
        //} else {warn!("header is empty"); 0};
        Utc::now()
    }

    pub fn append(&mut self, time: DateTime<Utc>, line: &[u8]) -> Result<(), Error> {
        //TODO decide if a lock is needed here

        //write 16 bit timestamp and then the line to file
        let timestamp = time.timestamp();
        self.i64_timestamp_to_u8_array(timestamp);

        self.data.write(&self.time_buffer[0..2])?;
        self.data.write(line)?;

        //write 64 bit timestamp to header
        //(happens no more then once every 18 hours)
        self.update_header(timestamp)?;

        self.force_write_to_disk();
        self.last_time_in_data = time;
        Ok(())
    }

    fn update_header(&mut self, timestamp: i64) -> Result<(), Error> {
        let timestamp_numb = timestamp / (u16::max_value() as i64);
        if timestamp_numb > self.header.last_timestamp_numb {
            println!("updating header");

            let line_start = self.data.metadata().unwrap().len() - self.line_size as u64;
            let mut line_start_buf = [0u8; 8];
            LittleEndian::write_u64(&mut line_start_buf, line_start);

            self.header.file.write(&self.time_buffer)?;
            self.header.file.write(&line_start_buf)?;
            self.header.data.insert(timestamp, line_start);

            self.header.last_timestamp_numb = timestamp_numb;
        }
        Ok(())
    }

    fn force_write_to_disk(&mut self) {
        self.data.sync_data().unwrap();
        self.header.file.sync_data().unwrap();
    }

	fn get_search_bounds(&mut self, start_time: DateTime<Utc>) -> (u64,u64) {
		//maximum in map less/equeal then/to needed timestamp
        let start_search = if let Some(start_byte) = self
            .header
            .data
            .range(..start_time.timestamp() + 1)
            .next_back()
        {
            *start_byte.1
        } else {
            //not found -> search till the end of file
            unimplemented!();
            0
        };
        //minimum in map greater then needed timestamp
        let stop_search =
            if let Some(stop_byte) = self.header.data.range(start_time.timestamp() + 1..).next() {
                *stop_byte.1
            } else {
                //no minimum greater then needed timestamp -> search till the end of file
                self.data.metadata().unwrap().len()
            };
        
        (start_search, stop_search)
	}

    fn set_read_start(&mut self, start_time: DateTime<Utc>) -> Result<(), Error> {
        //maximum in map less/equeal then/to needed timestamp
        let (start_search, stop_search) = self.get_search_bounds(start_time);

        //compare partial (16 bit) timestamps in between these bounds
        let mut buf = vec![0u8; (stop_search - start_search) as usize];
        self.data.seek(SeekFrom::Start(start_search)).unwrap();
        self.data.read_exact(&mut buf);

        let mut partial_timestamp = [0u8, 0u8];
        LittleEndian::write_u16(&mut partial_timestamp, start_time.timestamp() as u16);

        self.start_byte = stop_search;				
        for line_start in (0..buf.len()).step_by(self.line_size) {
            if Self::line_partial_timestamp_larger_then_given(&partial_timestamp, &buf[line_start..line_start + 2]) {
                if line_start >= self.line_size {//use previous value
					println!("setting start_byte from liniar search, pos: {}", line_start);
                    self.start_byte = start_search+ (line_start - self.line_size) as u64; break;
                } else { //at beginning of file use this value
					println!("setting start_byte from liniar search, start of search area");
                    self.start_byte = start_search+ line_start as u64; break;
                }
            }
        }
        self.data.seek(SeekFrom::Start(self.start_byte) );
        Ok(())
    }

    fn set_read_stop(&mut self, _end_time: DateTime<Utc>) {
        self.stop_byte = self.data.metadata().unwrap().len();
        error!("not implemented yet, reading to end of file");
    }

    fn line_partial_timestamp_larger_then_given(given: &[u8; 2], line: &[u8]) -> bool {
        if LittleEndian::read_u16(line) > LittleEndian::read_u16(given) {
            true
        } else {
            false
        }
    }

    fn i64_timestamp_to_u8_array(&mut self, x: i64) {
        //no support for sign bit since data will always be after 0 (1970)
        let x = if x < 0 { 0 } else { x as u64 };
        //we store the number in little endian Signed magnitude representation
        //(least significant (lowest value) byte at lowest adress)
        LittleEndian::write_u64(&mut self.time_buffer, x);
    }
    
    pub fn get_timestamp<T>(&mut self, line: &[u8], pos: u64) -> T 
    where T: FromPrimitive {

		//update full timestamp when needed
		if pos > self.header.next_timestamp_pos - 1 {			
			//update current timestamp
			self.header.current_timestamp = self.header.next_timestamp;
			
			//set next timestamp and timestamp pos
			//minimum in map greater then current timestamp			
			if let Some(next)
			= self.header.data.range(self.header.current_timestamp+1..).next() {
				self.header.next_timestamp = *next.0;
				self.header.next_timestamp_pos = *next.1;
			} else {
				//TODO handle edge case, last full timestamp
				self.header.next_timestamp = 0;
				self.header.next_timestamp_pos = u64::max_value();
			}
		}
		let timestamp_low = LittleEndian::read_u16(line) as u64;
		let timestamp_high = (self.header.current_timestamp as u64 >> 16) << 16;
		let timestamp = timestamp_high & timestamp_low;
		T::from_u64(timestamp).unwrap()
	}
    
}

impl Read for Timeseries {
	//guarantees we always discrete lines
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
		let mut nread = self.data.read(buf)?;

		nread = if self.start_byte+nread as u64 > self.stop_byte {
			println!("diff: {}, {}, {}",self.start_byte as i64, self.stop_byte as i64, self.stop_byte as i64- self.start_byte as i64);
			(self.stop_byte - self.start_byte) as usize
		} else {
			nread
		};
		self.start_byte += nread as u64;
		Ok(nread - nread % self.line_size)
    }
}

impl Timeseries {
	fn decode_sequential_time_only(&mut self, lines: usize) -> Result<(Vec<u32>, Vec<u8>), Error> {
		
		let mut buf = Vec::with_capacity(lines*self.line_size);
		
		let mut timestamps: Vec<u32>  = Vec::with_capacity(lines);
		let mut decoded: Vec<u8> = Vec::with_capacity(lines);
		
		let n_read = self.data.read(&mut buf).unwrap() as usize;
		let mut file_pos = self.start_byte;
		for (_i,line) in buf[..n_read].chunks(self.line_size).enumerate() {
			file_pos += self.line_size as u64;
			timestamps.push( self.get_timestamp::<u32>(line, file_pos));
			decoded.extend_from_slice(&line[2..]);
		}
		Ok((timestamps,decoded))
	}
}

//open file and check if it has the right lenght
//(an interger multiple of the line lenght) if it
//has not warn and repair by truncating
fn open_and_check(path: String, line_size: usize) -> Result<File, Error> {
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)?;
    let metadata = file.metadata()?;

    let rest = metadata.len() % (line_size as u64);
    if rest > 0 {
        warn!("Last write incomplete, truncating to largest multiple of the line size");
        file.set_len(metadata.len() - rest)?;
    }
    Ok(file)
}


//----------------------------------------------------------------------
//----------------------------------------------------------------------
#[macro_use]
extern crate log;
extern crate fxhash;

#[cfg(test)]
mod tests {
    use super::*;
    use self::chrono::{DateTime, NaiveDateTime, Utc};
	use std::fs;
	use std::path::Path;
	use self::byteorder::{NativeEndian, WriteBytesExt};
	use self::fxhash::hash64;

	fn insert_uniform_arrays(data: &mut Timeseries, n_to_insert: u32, _step: i64, time: DateTime<Utc>) {
		let mut timestamp = time.timestamp();
		for i in 0..n_to_insert {
			let buffer = [i as u8; 10];
			
			let dt = DateTime::<Utc>::from_utc(NaiveDateTime::from_timestamp(timestamp, 0), Utc);
			data.append(dt, &buffer).unwrap();
			timestamp += 5;
		}
	}
	
	fn insert_timestamp_hashes(data: &mut Timeseries, n_to_insert: u32, step: i64, time: DateTime<Utc>) {
		let mut timestamp = time.timestamp();
		let mut buffer = Vec::with_capacity(8);
			
		for _ in 0..n_to_insert {

			let _hash = hash64::<i64>(&timestamp);
			&buffer.write_u64::<NativeEndian>(143).unwrap();

			let dt = DateTime::<Utc>::from_utc(NaiveDateTime::from_timestamp(timestamp, 0), Utc);
			data.append(dt, buffer.as_slice()).unwrap();
			timestamp += step;
		}
	}

    #[test]
    fn test_set_read() {
				if Path::new("test_set_read.h").exists() {fs::remove_file("test_set_read.h").unwrap();}
				if Path::new("test_set_read.data").exists() {fs::remove_file("test_set_read.data").unwrap();}
				const LINE_SIZE: usize = 10; 
				const STEP: i64 = 5;
				
				let time = Utc::now(); let timestamp = time.timestamp();
				println!("now: {}, u8_ts: {}", time, timestamp as u8 );
				
        let mut data = Timeseries::open("test_set_read", LINE_SIZE).unwrap();
				insert_uniform_arrays(&mut data, 10, STEP, time);
				println!("data length: {}",data.data.metadata().unwrap().len());

				let t1 = DateTime::<Utc>::from_utc(NaiveDateTime::from_timestamp(timestamp+2*STEP, 0), Utc);
       
				data.set_read_start(t1).unwrap(); println!("start_byte: {}",data.start_byte);
        assert_eq!(data.start_byte, (data.line_size*2) as u64);
    }

    #[test]
    fn append_and_read() {
				if Path::new("test.h").exists() {fs::remove_file("test.h").unwrap();}
				if Path::new("test.data").exists() {fs::remove_file("test.data").unwrap();}
				
				let time = Utc::now();
				println!("now: {}, u8_ts: {}", time, time.timestamp() as u8 );
				
        let mut data = Timeseries::open("test", 10).unwrap();
				insert_uniform_arrays(&mut data, 10_000, 5, time);
				println!("data length: {}",data.data.metadata().unwrap().len());

				let timestamp = time.timestamp();
				let t1 = DateTime::<Utc>::from_utc(NaiveDateTime::from_timestamp(data.last_time_in_data.timestamp()-5, 0), Utc);
				let _t2 = DateTime::<Utc>::from_utc(NaiveDateTime::from_timestamp(timestamp+30, 0), Utc);
        
        data.set_read_start(t1).unwrap(); println!("start_byte: {}",data.start_byte);
        data.set_read_stop( Utc::now() ); println!("stop_byte: {}",data.stop_byte);

        let mut buffer = Vec::new();
        data.read_to_end(&mut buffer).unwrap();
        
        println!("buffer: {:?}", buffer);
        assert_eq!(2 + 2, 4);
    }

    #[test]
    fn long_append_and_read() {
		const NUMBER_TO_INSERT: i64 = 80_000; 
		const PERIOD: i64 = 5;
		
		if Path::new("test.h").exists() {fs::remove_file("test.h").unwrap();}
		if Path::new("test.data").exists() {fs::remove_file("test.data").unwrap();}
		
		let time = Utc::now();
				
        let mut data = Timeseries::open("test", 8).unwrap();
		insert_timestamp_hashes(&mut data, NUMBER_TO_INSERT as u32, PERIOD, time);

		let timestamp = time.timestamp();
		let t1 = DateTime::<Utc>::from_utc(NaiveDateTime::from_timestamp(timestamp, 0), Utc);
		let t2 = DateTime::<Utc>::from_utc(NaiveDateTime::from_timestamp(timestamp + NUMBER_TO_INSERT*PERIOD, 0), Utc);
        
        data.set_read_start(t1).unwrap(); println!("start_byte: {}",data.start_byte);
        data.set_read_stop( t2); println!("stop_byte: {}", data.stop_byte);
		assert_eq!((data.stop_byte-data.start_byte)/(data.line_size as u64), (NUMBER_TO_INSERT/2) as u64);

		let n = 8_000;
		let loops_to_check_everything = NUMBER_TO_INSERT/n + if NUMBER_TO_INSERT % n > 0 {1} else {0};
		for _ in 0..loops_to_check_everything {
			let (timestamps, decoded) = data.decode_sequential_time_only(n as usize).unwrap();
			for (timestamp, decoded) in timestamps.iter().zip(decoded.chunks(data.line_size)){
				let hash1 = hash64::<i64>(&(*timestamp as i64));
				let hash2 = NativeEndian::read_u64(decoded);
				assert_eq!(hash1, hash2);
			}
		}
        assert_eq!(2 + 2, 4);
    }

}
