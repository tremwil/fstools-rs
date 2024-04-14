use std::{
    char,
    io::{self, ErrorKind, Read},
    marker::PhantomData,
};

use byteorder::{ReadBytesExt, LE};
use flate2::read::ZlibDecoder;
use thiserror::Error;
use zerocopy::{FromBytes, FromZeroes, Ref, U32};

use crate::io_ext::{ReadFormatsExt, ReadWidestringError};

#[derive(Debug, Error)]
pub enum EntryFileListError {
    #[error("Could not read string")]
    String(#[from] ReadWidestringError),

    #[error("Could not create reference to value")]
    UnalignedValue,

    #[error("Zlib error")]
    Zlib,

    #[error("Io error")]
    Io(#[from] io::Error),
}

#[allow(unused)]
pub struct EntryFileList<'a> {
    container_header: &'a ContainerHeader,
    compressed: &'a [u8],
}

#[derive(FromZeroes, FromBytes, Debug)]
#[repr(packed)]
#[allow(unused)]
struct ContainerHeader {
    magic: [u8; 4],
    _unk04: U32<LE>,
    compressed_size: U32<LE>,
    decompressed_size: U32<LE>,
}

#[derive(Debug)]
pub struct EntryFileListHeader {
    pub _unk0: u32,
    pub unk1_count: usize,
    pub unk2_count: usize,
    pub _unkc: u32,
}

impl<'a> EntryFileList<'a> {
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, EntryFileListError> {
        let (container_header, compressed) = Ref::<_, ContainerHeader>::new_from_prefix(bytes)
            .ok_or(EntryFileListError::UnalignedValue)?;

        Ok(Self {
            container_header: container_header.into_ref(),
            compressed,
        })
    }

    pub fn content(&self) -> Result<SectionIter<'a, Unk1>, EntryFileListError> {
        let mut decoder = ZlibDecoder::new(self.compressed);

        let _unk0 = decoder.read_u32::<LE>()?;
        let unk1_count = decoder.read_u32::<LE>()? as usize;
        let unk2_count = decoder.read_u32::<LE>()? as usize;
        let _unkc = decoder.read_u32::<LE>()?;

        let header = EntryFileListHeader {
            _unk0,
            unk1_count,
            unk2_count,
            _unkc,
        };

        Ok(SectionIter {
            decoder,
            entry_count: header.unk1_count,
            header,
            entries_read: 0,
            _marker: PhantomData,
        })
    }
}

impl<'a> std::fmt::Debug for EntryFileList<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntryFileList")
            .field("unk04", &self.container_header._unk04.get())
            .field(
                "compressed_size",
                &self.container_header.compressed_size.get(),
            )
            .field(
                "decompressed_size",
                &self.container_header.decompressed_size.get(),
            )
            .finish()
    }
}

#[derive(Debug)]
pub struct SectionIter<'a, TElement> {
    decoder: ZlibDecoder<&'a [u8]>,
    header: EntryFileListHeader,
    entry_count: usize,
    entries_read: usize,
    _marker: PhantomData<TElement>,
}

impl<'a, TElement> SectionIter<'a, TElement> {
    fn skip_to_end(&mut self) -> io::Result<()> { 
        if self.entries_read != self.entry_count {
            let remaining = (self.entry_count - self.entries_read) * std::mem::size_of::<TElement>();
            std::io::copy(
                &mut self.decoder.by_ref().take(remaining as u64),
                &mut std::io::sink(),
            )?;
        }

        Ok(())
    }

    fn skip_to_alignment(&mut self) -> io::Result<()> {
        // Find distance to next alignment point if we're not already aligned
        let to_read = {
            let offset = self.decoder.total_out() as usize % 0x10;
            if offset == 0 {
                0
            } else {
                0x10 - offset
            }
        };

        self.decoder.read_padding(to_read)
    }
}

pub trait SectionElement: Sized {
   fn read(reader: &mut impl Read) -> std::io::Result<Self> where Self: Sized;
}

impl<'a, TElement> Iterator for SectionIter<'a, TElement>
where
    TElement: SectionElement,
{
    type Item = io::Result<TElement>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.entries_read < self.entry_count {
            let result = (self.entries_read..self.entry_count)
                .next()
                .map(|_| TElement::read(&mut self.decoder));

            self.entries_read += 1;

            result
        } else {
            None
        }
    }
}

#[derive(Debug)]
#[allow(unused)]
pub struct Unk1 {
    pub step: u16,
    pub index: u16,
}

impl SectionElement for Unk1 {
    fn read(reader: &mut impl Read) -> std::io::Result<Self> where Self: Sized {
        Ok(Unk1 {
            step: reader.read_u16::<LE>()?,
            index: reader.read_u16::<LE>()?,
        })
    }
}

impl<'a> SectionIter<'a, Unk1> {
    pub fn next_section(mut self) -> Result<SectionIter<'a, Unk2>, EntryFileListError> {
        self.skip_to_end()?;
        self.skip_to_alignment()?;

        Ok(SectionIter {
            decoder: self.decoder,
            entry_count: self.header.unk2_count,
            header: self.header,
            entries_read: 0,
            _marker: PhantomData,
        })
    }
}

#[derive(Debug)]
#[allow(unused)]
pub struct Unk2(u64);

impl SectionElement for Unk2 {
    fn read(reader: &mut impl Read) -> std::io::Result<Self> where Self: Sized {
        Ok(Unk2(reader.read_u64::<LE>()?))
    }
}

#[derive(Debug)]
#[allow(unused)]
pub struct UnkString(String);

impl SectionElement for UnkString {
    fn read(reader: &mut impl Read) -> std::io::Result<Self> where Self: Sized {
        let mut string = String::new();

        loop {
            let c = reader.read_u16::<LE>()?;
            if c == 0x0 {
                break;
            }

            string.push(char::from_u32(c as u32).ok_or(io::Error::from(ErrorKind::InvalidData))?);
        }

        Ok(UnkString(string))
    }
}

impl<'a> SectionIter<'a, Unk2> {
    pub fn next_section(mut self) -> Result<SectionIter<'a, UnkString>, EntryFileListError> {
        self.skip_to_end()?;
        self.skip_to_alignment()?;

        // This is seems to be some value since the unk2 count excludes this
        // if it were a string. It's always 0x0000 though so :shrug:
        self.decoder.read_padding(2)?;

        Ok(SectionIter {
            decoder: self.decoder,
            entry_count: self.header.unk2_count,
            header: self.header,
            entries_read: 0,
            _marker: PhantomData,
        })
    }
}
