//! SPA POD (Simple Plugin API Plain Old Data) serializer and deserializer.
//!
//! Complies with PipeWire's 8-byte aligned SPA POD specification.

use thiserror::Error;

pub const SPA_TYPE_NONE: u32 = 1;
pub const SPA_TYPE_BOOL: u32 = 2;
pub const SPA_TYPE_ID: u32 = 3;
pub const SPA_TYPE_INT: u32 = 4;
pub const SPA_TYPE_LONG: u32 = 5;
pub const SPA_TYPE_FLOAT: u32 = 6;
pub const SPA_TYPE_DOUBLE: u32 = 7;
pub const SPA_TYPE_STRING: u32 = 8;
pub const SPA_TYPE_BYTES: u32 = 9;
pub const SPA_TYPE_RECTANGLE: u32 = 10;
pub const SPA_TYPE_FRACTION: u32 = 11;
pub const SPA_TYPE_BITMAP: u32 = 12;
pub const SPA_TYPE_ARRAY: u32 = 13;
pub const SPA_TYPE_STRUCT: u32 = 14;
pub const SPA_TYPE_OBJECT: u32 = 15;
pub const SPA_TYPE_SEQUENCE: u32 = 16;
pub const SPA_TYPE_POINTER: u32 = 17;
pub const SPA_TYPE_FD: u32 = 18;
pub const SPA_TYPE_CHOICE: u32 = 19;
pub const SPA_TYPE_POD: u32 = 20;

pub const SPA_CHOICE_NONE: u32 = 0;
pub const SPA_CHOICE_RANGE: u32 = 1;
pub const SPA_CHOICE_STEP: u32 = 2;
pub const SPA_CHOICE_ENUM: u32 = 3;
pub const SPA_CHOICE_FLAGS: u32 = 4;

pub const SPA_TYPE_OBJECT_PROPS: u32 = 0x40002;
pub const SPA_TYPE_OBJECT_FORMAT: u32 = 0x40003;
pub const SPA_TYPE_OBJECT_PARAM_BUFFERS: u32 = 0x40004;
pub const SPA_TYPE_OBJECT_PARAM_IO: u32 = 0x40006;

pub const SPA_PARAM_PROPS: u32 = 2;
pub const SPA_PARAM_ENUM_FORMAT: u32 = 3;
pub const SPA_PARAM_FORMAT: u32 = 4;
pub const SPA_PARAM_BUFFERS: u32 = 5;
pub const SPA_PARAM_IO: u32 = 7;

pub const SPA_MEDIA_TYPE_AUDIO: u32 = 1;
pub const SPA_MEDIA_SUBTYPE_DSP: u32 = 2;
pub const SPA_AUDIO_FORMAT_DSP_F32: u32 = 518;
pub const SPA_AUDIO_FORMAT_F32P: u32 = 518;
pub const SPA_FORMAT_MEDIA_TYPE: u32 = 1;
pub const SPA_FORMAT_MEDIA_SUBTYPE: u32 = 2;
pub const SPA_MEDIA_SUBTYPE_RAW: u32 = 1;
pub const SPA_FORMAT_AUDIO_FORMAT: u32 = 65537;
pub const SPA_FORMAT_AUDIO_RATE: u32 = 65539;
pub const SPA_FORMAT_AUDIO_CHANNELS: u32 = 65540;
pub const SPA_FORMAT_AUDIO_POSITION: u32 = 65541;

pub const SPA_AUDIO_CHANNEL_FL: u32 = 3;
pub const SPA_AUDIO_CHANNEL_FR: u32 = 4;

pub const SPA_PARAM_INFO_SERIAL: u32 = 1 << 0;
pub const SPA_PARAM_INFO_READ: u32 = 1 << 1;
pub const SPA_PARAM_INFO_WRITE: u32 = 1 << 2;
pub const SPA_PARAM_INFO_READWRITE: u32 = SPA_PARAM_INFO_READ | SPA_PARAM_INFO_WRITE;

pub const SPA_PARAM_BUFFERS_BUFFERS: u32 = 1;
pub const SPA_PARAM_BUFFERS_BLOCKS: u32 = 2;
pub const SPA_PARAM_BUFFERS_SIZE: u32 = 3;
pub const SPA_PARAM_BUFFERS_STRIDE: u32 = 4;
pub const SPA_PARAM_BUFFERS_DATA_TYPE: u32 = 6;
pub const SPA_DATA_MEM_PTR: u32 = 1;
pub const SPA_DATA_MEM_FD: u32 = 2;
pub const SPA_DATA_MEM_ID: u32 = 4;

pub const SPA_PARAM_IO_ID: u32 = 1;
pub const SPA_PARAM_IO_SIZE: u32 = 2;
pub const SPA_IO_BUFFERS: u32 = 1;
pub const SPA_IO_CLOCK: u32 = 3;
pub const SPA_IO_POSITION: u32 = 7;

#[inline]
pub fn align8(v: usize) -> usize {
    (v + 7) & !7
}

#[inline]
pub fn pad8(v: usize) -> usize {
    (8 - (v % 8)) % 8
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum PodError {
    #[error("Buffer too short (need at least {need}, have {have})")]
    UnexpectedEof { need: usize, have: usize },
    #[error("Unknown POD type {0}")]
    UnknownType(u32),
    #[error("Invalid POD size {size} for type {type_}")]
    InvalidSize { type_: u32, size: u32 },
    #[error("Invalid UTF-8 string")]
    InvalidUtf8,
    #[error("Trailing garbage")]
    TrailingGarbage,
    #[error("Invalid dictionary structure")]
    InvalidDict,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Property {
    pub key: u32,
    pub flags: u32,
    pub value: PodValue,
}

#[derive(Clone, PartialEq, Debug)]
pub enum PodValue {
    None,
    Bool(bool),
    Id(u32),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    String(String),
    Bytes(Vec<u8>),
    Rectangle {
        width: u32,
        height: u32,
    },
    Fraction {
        num: u32,
        denom: u32,
    },
    Array {
        child_type: u32,
        values: Vec<PodValue>,
    },
    Struct(Vec<PodValue>),
    Object {
        type_: u32,
        id: u32,
        props: Vec<Property>,
    },
    Choice {
        choice_type: u32,
        flags: u32,
        values: Vec<PodValue>,
    },
    Pointer {
        type_: u32,
        value: u64,
    },
    Fd(i64),
}

impl PodValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            PodValue::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_i32(&self) -> Option<i32> {
        match self {
            PodValue::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            PodValue::Id(v) => Some(*v),
            PodValue::Int(v) if *v >= 0 => Some(*v as u32),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            PodValue::Long(v) => Some(*v),
            PodValue::Fd(v) => Some(*v),
            PodValue::Int(v) => Some(*v as i64),
            _ => None,
        }
    }

    pub fn as_struct(&self) -> Option<&[PodValue]> {
        match self {
            PodValue::Struct(s) => Some(s.as_slice()),
            _ => None,
        }
    }
}

pub struct StructFrame {
    start_offset: usize,
}

pub struct ObjectFrame {
    start_offset: usize,
}

#[derive(Default, Clone)]
pub struct PodBuilder {
    buf: Vec<u8>,
}

impl PodBuilder {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(256),
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn write_raw(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    fn write_header(&mut self, size: u32, type_: u32) {
        self.buf.extend_from_slice(&size.to_ne_bytes());
        self.buf.extend_from_slice(&type_.to_ne_bytes());
    }

    fn write_padding(&mut self, body_len: usize) {
        let pad = pad8(body_len);
        for _ in 0..pad {
            self.buf.push(0);
        }
    }

    pub fn write_none(&mut self) {
        self.write_header(0, SPA_TYPE_NONE);
    }

    pub fn write_bool(&mut self, val: bool) {
        self.write_header(4, SPA_TYPE_BOOL);
        let v: i32 = if val { 1 } else { 0 };
        self.buf.extend_from_slice(&v.to_ne_bytes());
        self.buf.extend_from_slice(&0u32.to_ne_bytes()); // padding
    }

    pub fn write_id(&mut self, val: u32) {
        self.write_header(4, SPA_TYPE_ID);
        self.buf.extend_from_slice(&val.to_ne_bytes());
        self.buf.extend_from_slice(&0u32.to_ne_bytes()); // padding
    }

    pub fn write_int(&mut self, val: i32) {
        self.write_header(4, SPA_TYPE_INT);
        self.buf.extend_from_slice(&val.to_ne_bytes());
        self.buf.extend_from_slice(&0u32.to_ne_bytes()); // padding
    }

    pub fn write_long(&mut self, val: i64) {
        self.write_header(8, SPA_TYPE_LONG);
        self.buf.extend_from_slice(&val.to_ne_bytes());
    }

    pub fn write_float(&mut self, val: f32) {
        self.write_header(4, SPA_TYPE_FLOAT);
        self.buf.extend_from_slice(&val.to_ne_bytes());
        self.buf.extend_from_slice(&0u32.to_ne_bytes()); // padding
    }

    pub fn write_double(&mut self, val: f64) {
        self.write_header(8, SPA_TYPE_DOUBLE);
        self.buf.extend_from_slice(&val.to_ne_bytes());
    }

    pub fn write_string(&mut self, val: &str) {
        let bytes = val.as_bytes();
        let size = (bytes.len() + 1) as u32; // includes null terminator
        self.write_header(size, SPA_TYPE_STRING);
        self.buf.extend_from_slice(bytes);
        self.buf.push(0); // null terminator
        self.write_padding(size as usize);
    }

    pub fn write_bytes(&mut self, val: &[u8]) {
        let size = val.len() as u32;
        self.write_header(size, SPA_TYPE_BYTES);
        self.buf.extend_from_slice(val);
        self.write_padding(size as usize);
    }

    pub fn write_rectangle(&mut self, width: u32, height: u32) {
        self.write_header(8, SPA_TYPE_RECTANGLE);
        self.buf.extend_from_slice(&width.to_ne_bytes());
        self.buf.extend_from_slice(&height.to_ne_bytes());
    }

    pub fn write_fraction(&mut self, num: u32, denom: u32) {
        self.write_header(8, SPA_TYPE_FRACTION);
        self.buf.extend_from_slice(&num.to_ne_bytes());
        self.buf.extend_from_slice(&denom.to_ne_bytes());
    }

    pub fn write_fd(&mut self, val: i64) {
        self.write_header(8, SPA_TYPE_FD);
        self.buf.extend_from_slice(&val.to_ne_bytes());
    }

    pub fn write_pointer(&mut self, type_: u32, value: u64) {
        self.write_header(16, SPA_TYPE_POINTER);
        self.buf.extend_from_slice(&type_.to_ne_bytes());
        self.buf.extend_from_slice(&0u32.to_ne_bytes()); // padding
        self.buf.extend_from_slice(&value.to_ne_bytes());
    }

    pub fn push_struct(&mut self) -> StructFrame {
        let start = self.buf.len();
        self.write_header(0, SPA_TYPE_STRUCT); // size patched in pop_struct
        StructFrame {
            start_offset: start,
        }
    }

    pub fn pop_struct(&mut self, frame: StructFrame) {
        let body_len = (self.buf.len() - frame.start_offset - 8) as u32;
        let size_bytes = body_len.to_ne_bytes();
        self.buf[frame.start_offset..frame.start_offset + 4].copy_from_slice(&size_bytes);
        // Struct contents are already padded since each child POD is padded to 8 bytes.
    }

    pub fn push_object(&mut self, type_: u32, id: u32) -> ObjectFrame {
        let start = self.buf.len();
        self.write_header(0, SPA_TYPE_OBJECT); // size patched in pop_object
        self.buf.extend_from_slice(&type_.to_ne_bytes());
        self.buf.extend_from_slice(&id.to_ne_bytes());
        ObjectFrame {
            start_offset: start,
        }
    }

    pub fn write_prop(&mut self, key: u32, flags: u32) {
        self.buf.extend_from_slice(&key.to_ne_bytes());
        self.buf.extend_from_slice(&flags.to_ne_bytes());
    }

    pub fn pop_object(&mut self, frame: ObjectFrame) {
        let body_len = (self.buf.len() - frame.start_offset - 8) as u32;
        let size_bytes = body_len.to_ne_bytes();
        self.buf[frame.start_offset..frame.start_offset + 4].copy_from_slice(&size_bytes);
    }

    pub fn write_choice_range_int(&mut self, default: i32, min: i32, max: i32) {
        // Body: type (u32), flags (u32), child (size: 4, type: 4), default (i32), min (i32), max (i32) = 28 bytes
        let body_len: u32 = 4 + 4 + 8 + 4 + 4 + 4;
        self.write_header(body_len, SPA_TYPE_CHOICE);
        self.buf.extend_from_slice(&SPA_CHOICE_RANGE.to_ne_bytes());
        self.buf.extend_from_slice(&0u32.to_ne_bytes()); // flags = 0
        // Child pod description
        self.buf.extend_from_slice(&4u32.to_ne_bytes()); // child.size = 4
        self.buf.extend_from_slice(&SPA_TYPE_INT.to_ne_bytes()); // child.type = Int
        // Values
        self.buf.extend_from_slice(&default.to_ne_bytes());
        self.buf.extend_from_slice(&min.to_ne_bytes());
        self.buf.extend_from_slice(&max.to_ne_bytes());
        self.write_padding(body_len as usize);
    }

    pub fn write_choice_flags_int(&mut self, flags_val: u32) {
        // Body: type (u32), flags (u32), child (size: 4, type: 4), flags_val (u32) = 20 bytes
        let body_len: u32 = 4 + 4 + 8 + 4;
        self.write_header(body_len, SPA_TYPE_CHOICE);
        self.buf.extend_from_slice(&SPA_CHOICE_FLAGS.to_ne_bytes());
        self.buf.extend_from_slice(&0u32.to_ne_bytes()); // flags = 0
        // Child pod description
        self.buf.extend_from_slice(&4u32.to_ne_bytes());
        self.buf.extend_from_slice(&SPA_TYPE_INT.to_ne_bytes());
        // Value
        self.buf.extend_from_slice(&flags_val.to_ne_bytes());
        self.write_padding(body_len as usize);
    }

    pub fn write_choice_step_int(&mut self, default: i32, min: i32, max: i32, step: i32) {
        let body_len: u32 = 4 + 4 + 8 + 16;
        self.write_header(body_len, SPA_TYPE_CHOICE);
        self.buf.extend_from_slice(&SPA_CHOICE_STEP.to_ne_bytes());
        self.buf.extend_from_slice(&0u32.to_ne_bytes());
        self.buf.extend_from_slice(&4u32.to_ne_bytes());
        self.buf.extend_from_slice(&SPA_TYPE_INT.to_ne_bytes());
        self.buf.extend_from_slice(&default.to_ne_bytes());
        self.buf.extend_from_slice(&min.to_ne_bytes());
        self.buf.extend_from_slice(&max.to_ne_bytes());
        self.buf.extend_from_slice(&step.to_ne_bytes());
        self.write_padding(body_len as usize);
    }

    /// Write dictionary items inline without an enclosing struct: Int(n_items), String(k1), String(v1)...
    pub fn write_dict_items(&mut self, items: &[(&str, &str)]) {
        self.write_int(items.len() as i32);
        for &(k, v) in items {
            self.write_string(k);
            self.write_string(v);
        }
    }

    /// Write dictionary struct: Struct { Int(n_items), String(k1), String(v1)... }
    pub fn write_dict(&mut self, items: &[(&str, &str)]) {
        let frame = self.push_struct();
        self.write_dict_items(items);
        self.pop_struct(frame);
    }

    /// Write an array of IDs: Array { child.size: 4, child.type: Id, ids... }
    pub fn write_array_id(&mut self, ids: &[u32]) {
        let body_len: u32 = 8 + (ids.len() * 4) as u32;
        self.write_header(body_len, SPA_TYPE_ARRAY);
        self.buf.extend_from_slice(&4u32.to_ne_bytes());
        self.buf.extend_from_slice(&SPA_TYPE_ID.to_ne_bytes());
        for &id in ids {
            self.buf.extend_from_slice(&id.to_ne_bytes());
        }
        self.write_padding(body_len as usize);
    }

    /// Write an array of floats: Array { child.size: 4, child.type: Float, values... }
    pub fn write_array_float(&mut self, values: &[f32]) {
        let body_len: u32 = 8 + (values.len() * 4) as u32;
        self.write_header(body_len, SPA_TYPE_ARRAY);
        self.buf.extend_from_slice(&4u32.to_ne_bytes());
        self.buf.extend_from_slice(&SPA_TYPE_FLOAT.to_ne_bytes());
        for &v in values {
            self.buf.extend_from_slice(&v.to_ne_bytes());
        }
        self.write_padding(body_len as usize);
    }

    pub fn write_value(&mut self, value: &PodValue) {
        match value {
            PodValue::None => self.write_none(),
            PodValue::Bool(b) => self.write_bool(*b),
            PodValue::Id(id) => self.write_id(*id),
            PodValue::Int(i) => self.write_int(*i),
            PodValue::Long(l) => self.write_long(*l),
            PodValue::Float(f) => self.write_float(*f),
            PodValue::Double(d) => self.write_double(*d),
            PodValue::String(s) => self.write_string(s),
            PodValue::Bytes(b) => self.write_bytes(b),
            PodValue::Rectangle { width, height } => self.write_rectangle(*width, *height),
            PodValue::Fraction { num, denom } => self.write_fraction(*num, *denom),
            PodValue::Array { child_type, values } => {
                let elem_size = match *child_type {
                    SPA_TYPE_INT | SPA_TYPE_ID | SPA_TYPE_FLOAT => 4,
                    SPA_TYPE_LONG | SPA_TYPE_DOUBLE | SPA_TYPE_RECTANGLE | SPA_TYPE_FRACTION => 8,
                    _ => 4,
                };
                let body_len = (8 + values.len() * elem_size) as u32;
                self.write_header(body_len, SPA_TYPE_ARRAY);
                self.buf
                    .extend_from_slice(&(elem_size as u32).to_ne_bytes());
                self.buf.extend_from_slice(&child_type.to_ne_bytes());
                for val in values {
                    match val {
                        PodValue::Int(v) => self.buf.extend_from_slice(&v.to_ne_bytes()),
                        PodValue::Id(v) => self.buf.extend_from_slice(&v.to_ne_bytes()),
                        PodValue::Float(v) => self.buf.extend_from_slice(&v.to_ne_bytes()),
                        PodValue::Long(v) => self.buf.extend_from_slice(&v.to_ne_bytes()),
                        _ => {}
                    }
                }
                self.write_padding(body_len as usize);
            }
            PodValue::Struct(items) => {
                let frame = self.push_struct();
                for item in items {
                    self.write_value(item);
                }
                self.pop_struct(frame);
            }
            PodValue::Object { type_, id, props } => {
                let frame = self.push_object(*type_, *id);
                for prop in props {
                    self.write_prop(prop.key, prop.flags);
                    self.write_value(&prop.value);
                }
                self.pop_object(frame);
            }
            PodValue::Choice {
                choice_type,
                flags,
                values,
            } => {
                if *choice_type == SPA_CHOICE_RANGE && values.len() == 3 {
                    if let (Some(def), Some(min), Some(max)) =
                        (values[0].as_i32(), values[1].as_i32(), values[2].as_i32())
                    {
                        self.write_choice_range_int(def, min, max);
                        return;
                    }
                }
                if *choice_type == SPA_CHOICE_FLAGS && !values.is_empty() {
                    if let Some(f) = values[0].as_u32() {
                        self.write_choice_flags_int(f);
                        return;
                    }
                }
                let body_len = (16 + values.len() * 4) as u32;
                self.write_header(body_len, SPA_TYPE_CHOICE);
                self.buf.extend_from_slice(&choice_type.to_ne_bytes());
                self.buf.extend_from_slice(&flags.to_ne_bytes());
                self.buf.extend_from_slice(&4u32.to_ne_bytes());
                self.buf.extend_from_slice(&SPA_TYPE_INT.to_ne_bytes());
                for v in values {
                    if let Some(i) = v.as_i32() {
                        self.buf.extend_from_slice(&i.to_ne_bytes());
                    }
                }
                self.write_padding(body_len as usize);
            }
            PodValue::Pointer { type_, value } => self.write_pointer(*type_, *value),
            PodValue::Fd(fd) => self.write_fd(*fd),
        }
    }
}

pub struct PodParser<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> PodParser<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.offset
    }

    pub fn is_empty(&self) -> bool {
        self.offset >= self.data.len()
    }

    fn read_u32(&mut self) -> Result<u32, PodError> {
        if self.remaining() < 4 {
            return Err(PodError::UnexpectedEof {
                need: 4,
                have: self.remaining(),
            });
        }
        let bytes: [u8; 4] = self.data[self.offset..self.offset + 4].try_into().unwrap();
        self.offset += 4;
        Ok(u32::from_ne_bytes(bytes))
    }

    #[allow(dead_code)]
    fn read_i32(&mut self) -> Result<i32, PodError> {
        self.read_u32().map(|u| u as i32)
    }

    #[allow(dead_code)]
    fn read_u64(&mut self) -> Result<u64, PodError> {
        if self.remaining() < 8 {
            return Err(PodError::UnexpectedEof {
                need: 8,
                have: self.remaining(),
            });
        }
        let bytes: [u8; 8] = self.data[self.offset..self.offset + 8].try_into().unwrap();
        self.offset += 8;
        Ok(u64::from_ne_bytes(bytes))
    }

    #[allow(dead_code)]
    fn read_i64(&mut self) -> Result<i64, PodError> {
        self.read_u64().map(|u| u as i64)
    }

    #[allow(dead_code)]
    fn read_f32(&mut self) -> Result<f32, PodError> {
        if self.remaining() < 4 {
            return Err(PodError::UnexpectedEof {
                need: 4,
                have: self.remaining(),
            });
        }
        let bytes: [u8; 4] = self.data[self.offset..self.offset + 4].try_into().unwrap();
        self.offset += 4;
        Ok(f32::from_ne_bytes(bytes))
    }

    #[allow(dead_code)]
    fn read_f64(&mut self) -> Result<f64, PodError> {
        if self.remaining() < 8 {
            return Err(PodError::UnexpectedEof {
                need: 8,
                have: self.remaining(),
            });
        }
        let bytes: [u8; 8] = self.data[self.offset..self.offset + 8].try_into().unwrap();
        self.offset += 8;
        Ok(f64::from_ne_bytes(bytes))
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<PodValue, PodError> {
        let size = self.read_u32()? as usize;
        let type_ = self.read_u32()?;
        let padded_size = align8(size);

        if self.remaining() < padded_size {
            return Err(PodError::UnexpectedEof {
                need: padded_size,
                have: self.remaining(),
            });
        }

        let body = &self.data[self.offset..self.offset + size];
        let next_offset = self.offset + padded_size;

        let val = match type_ {
            SPA_TYPE_NONE => PodValue::None,
            SPA_TYPE_BOOL => {
                if size < 4 {
                    return Err(PodError::InvalidSize {
                        type_,
                        size: size as u32,
                    });
                }
                let v = i32::from_ne_bytes(body[0..4].try_into().unwrap());
                PodValue::Bool(v != 0)
            }
            SPA_TYPE_ID => {
                if size < 4 {
                    return Err(PodError::InvalidSize {
                        type_,
                        size: size as u32,
                    });
                }
                let v = u32::from_ne_bytes(body[0..4].try_into().unwrap());
                PodValue::Id(v)
            }
            SPA_TYPE_INT => {
                if size < 4 {
                    return Err(PodError::InvalidSize {
                        type_,
                        size: size as u32,
                    });
                }
                let v = i32::from_ne_bytes(body[0..4].try_into().unwrap());
                PodValue::Int(v)
            }
            SPA_TYPE_LONG => {
                if size < 8 {
                    return Err(PodError::InvalidSize {
                        type_,
                        size: size as u32,
                    });
                }
                let v = i64::from_ne_bytes(body[0..8].try_into().unwrap());
                PodValue::Long(v)
            }
            SPA_TYPE_FLOAT => {
                if size < 4 {
                    return Err(PodError::InvalidSize {
                        type_,
                        size: size as u32,
                    });
                }
                let v = f32::from_ne_bytes(body[0..4].try_into().unwrap());
                PodValue::Float(v)
            }
            SPA_TYPE_DOUBLE => {
                if size < 8 {
                    return Err(PodError::InvalidSize {
                        type_,
                        size: size as u32,
                    });
                }
                let v = f64::from_ne_bytes(body[0..8].try_into().unwrap());
                PodValue::Double(v)
            }
            SPA_TYPE_STRING => {
                // Remove trailing null terminator if present
                let s_bytes = if !body.is_empty() && body[body.len() - 1] == 0 {
                    &body[..body.len() - 1]
                } else {
                    body
                };
                let s = std::str::from_utf8(s_bytes).map_err(|_| PodError::InvalidUtf8)?;
                PodValue::String(s.to_string())
            }
            SPA_TYPE_BYTES => PodValue::Bytes(body.to_vec()),
            SPA_TYPE_RECTANGLE => {
                if size < 8 {
                    return Err(PodError::InvalidSize {
                        type_,
                        size: size as u32,
                    });
                }
                let w = u32::from_ne_bytes(body[0..4].try_into().unwrap());
                let h = u32::from_ne_bytes(body[4..8].try_into().unwrap());
                PodValue::Rectangle {
                    width: w,
                    height: h,
                }
            }
            SPA_TYPE_FRACTION => {
                if size < 8 {
                    return Err(PodError::InvalidSize {
                        type_,
                        size: size as u32,
                    });
                }
                let num = u32::from_ne_bytes(body[0..4].try_into().unwrap());
                let denom = u32::from_ne_bytes(body[4..8].try_into().unwrap());
                PodValue::Fraction { num, denom }
            }
            SPA_TYPE_FD => {
                if size < 8 {
                    return Err(PodError::InvalidSize {
                        type_,
                        size: size as u32,
                    });
                }
                let v = i64::from_ne_bytes(body[0..8].try_into().unwrap());
                PodValue::Fd(v)
            }
            SPA_TYPE_POINTER => {
                if size < 16 {
                    return Err(PodError::InvalidSize {
                        type_,
                        size: size as u32,
                    });
                }
                let ptype = u32::from_ne_bytes(body[0..4].try_into().unwrap());
                let val = u64::from_ne_bytes(body[8..16].try_into().unwrap());
                PodValue::Pointer {
                    type_: ptype,
                    value: val,
                }
            }
            SPA_TYPE_STRUCT => {
                let mut inner_parser = PodParser::new(body);
                let mut items = Vec::new();
                while !inner_parser.is_empty() {
                    items.push(inner_parser.next()?);
                }
                PodValue::Struct(items)
            }
            SPA_TYPE_OBJECT => {
                if size < 8 {
                    return Err(PodError::InvalidSize {
                        type_,
                        size: size as u32,
                    });
                }
                let obj_type = u32::from_ne_bytes(body[0..4].try_into().unwrap());
                let obj_id = u32::from_ne_bytes(body[4..8].try_into().unwrap());
                let mut props = Vec::new();
                let mut pos = 8;
                while pos + 8 <= body.len() {
                    let key = u32::from_ne_bytes(body[pos..pos + 4].try_into().unwrap());
                    let flags = u32::from_ne_bytes(body[pos + 4..pos + 8].try_into().unwrap());
                    pos += 8;
                    let mut prop_parser = PodParser::new(&body[pos..]);
                    let val = prop_parser.next()?;
                    let val_len = prop_parser.offset;
                    pos += val_len;
                    props.push(Property {
                        key,
                        flags,
                        value: val,
                    });
                }
                PodValue::Object {
                    type_: obj_type,
                    id: obj_id,
                    props,
                }
            }
            SPA_TYPE_CHOICE => {
                if size < 16 {
                    return Err(PodError::InvalidSize {
                        type_,
                        size: size as u32,
                    });
                }
                let choice_type = u32::from_ne_bytes(body[0..4].try_into().unwrap());
                let flags = u32::from_ne_bytes(body[4..8].try_into().unwrap());
                let child_size = u32::from_ne_bytes(body[8..12].try_into().unwrap()) as usize;
                let child_type = u32::from_ne_bytes(body[12..16].try_into().unwrap());
                let mut values = Vec::new();
                let mut pos = 16;
                while pos + child_size <= size {
                    let val_bytes = &body[pos..pos + child_size];
                    let val = match child_type {
                        SPA_TYPE_INT => {
                            if child_size >= 4 {
                                PodValue::Int(i32::from_ne_bytes(
                                    val_bytes[0..4].try_into().unwrap(),
                                ))
                            } else {
                                break;
                            }
                        }
                        SPA_TYPE_ID => {
                            if child_size >= 4 {
                                PodValue::Id(u32::from_ne_bytes(
                                    val_bytes[0..4].try_into().unwrap(),
                                ))
                            } else {
                                break;
                            }
                        }
                        SPA_TYPE_FLOAT => {
                            if child_size >= 4 {
                                PodValue::Float(f32::from_ne_bytes(
                                    val_bytes[0..4].try_into().unwrap(),
                                ))
                            } else {
                                break;
                            }
                        }
                        _ => break,
                    };
                    values.push(val);
                    pos += child_size;
                }
                PodValue::Choice {
                    choice_type,
                    flags,
                    values,
                }
            }
            SPA_TYPE_ARRAY => {
                if size < 8 {
                    return Err(PodError::InvalidSize {
                        type_,
                        size: size as u32,
                    });
                }
                let child_size = u32::from_ne_bytes(body[0..4].try_into().unwrap()) as usize;
                let child_type = u32::from_ne_bytes(body[4..8].try_into().unwrap());
                let mut values = Vec::new();
                let mut pos = 8;
                while pos + child_size <= size {
                    let val_bytes = &body[pos..pos + child_size];
                    let val = match child_type {
                        SPA_TYPE_INT if child_size >= 4 => {
                            PodValue::Int(i32::from_ne_bytes(val_bytes[0..4].try_into().unwrap()))
                        }
                        SPA_TYPE_ID if child_size >= 4 => {
                            PodValue::Id(u32::from_ne_bytes(val_bytes[0..4].try_into().unwrap()))
                        }
                        SPA_TYPE_FLOAT if child_size >= 4 => {
                            PodValue::Float(f32::from_ne_bytes(val_bytes[0..4].try_into().unwrap()))
                        }
                        _ => break,
                    };
                    values.push(val);
                    pos += child_size;
                }
                PodValue::Array { child_type, values }
            }
            other => return Err(PodError::UnknownType(other)),
        };

        self.offset = next_offset;
        Ok(val)
    }

    /// Extract key-value dictionary from a dict struct:
    /// Struct { Int(n_items), String(k1), String(v1), ... }
    pub fn parse_dict(val: &PodValue) -> Result<Vec<(String, String)>, PodError> {
        let items = match val {
            PodValue::Struct(s) => s,
            _ => return Err(PodError::InvalidDict),
        };
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let n_items = items[0].as_i32().ok_or(PodError::InvalidDict)? as usize;
        let mut dict = Vec::with_capacity(n_items);
        let mut idx = 1;
        for _ in 0..n_items {
            if idx + 1 >= items.len() {
                break;
            }
            let key = items[idx].as_str().ok_or(PodError::InvalidDict)?;
            let value = items[idx + 1].as_str().ok_or(PodError::InvalidDict)?;
            dict.push((key.to_string(), value.to_string()));
            idx += 2;
        }
        Ok(dict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_byte_exact_vectors() {
        // 1. None
        let mut b = PodBuilder::new();
        b.write_none();
        assert_eq!(
            b.as_bytes(),
            &[0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]
        );

        // 2. Bool true
        let mut b = PodBuilder::new();
        b.write_bool(true);
        assert_eq!(
            b.as_bytes(),
            &[
                0x04, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00
            ]
        );

        // 3. Id 123
        let mut b = PodBuilder::new();
        b.write_id(123);
        assert_eq!(
            b.as_bytes(),
            &[
                0x04, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x7b, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00
            ]
        );

        // 4. Int -456
        let mut b = PodBuilder::new();
        b.write_int(-456);
        assert_eq!(
            b.as_bytes(),
            &[
                0x04, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x38, 0xfe, 0xff, 0xff, 0x00, 0x00,
                0x00, 0x00
            ]
        );

        // 5. Long
        let mut b = PodBuilder::new();
        b.write_long(0x123456789abcdef0u64 as i64);
        assert_eq!(
            b.as_bytes(),
            &[
                0x08, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0xf0, 0xde, 0xbc, 0x9a, 0x78, 0x56,
                0x34, 0x12
            ]
        );

        // 6. Float 1.5
        let mut b = PodBuilder::new();
        b.write_float(1.5f32);
        assert_eq!(
            b.as_bytes(),
            &[
                0x04, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x3f, 0x00, 0x00,
                0x00, 0x00
            ]
        );

        // 7. String 'hello'
        let mut b = PodBuilder::new();
        b.write_string("hello");
        assert_eq!(
            b.as_bytes(),
            &[
                0x06, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x00,
                0x00, 0x00
            ]
        );

        // 8. Fraction 48000/1
        let mut b = PodBuilder::new();
        b.write_fraction(48000, 1);
        assert_eq!(
            b.as_bytes(),
            &[
                0x08, 0x00, 0x00, 0x00, 0x0b, 0x00, 0x00, 0x00, 0x80, 0xbb, 0x00, 0x00, 0x01, 0x00,
                0x00, 0x00
            ]
        );

        // 9. Rectangle 1920x1080
        let mut b = PodBuilder::new();
        b.write_rectangle(1920, 1080);
        assert_eq!(
            b.as_bytes(),
            &[
                0x08, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x80, 0x07, 0x00, 0x00, 0x38, 0x04,
                0x00, 0x00
            ]
        );

        // 10. Fd 7
        let mut b = PodBuilder::new();
        b.write_fd(7);
        assert_eq!(
            b.as_bytes(),
            &[
                0x08, 0x00, 0x00, 0x00, 0x12, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00
            ]
        );

        // 11. Struct [Int(42), String('hi')]
        let mut b = PodBuilder::new();
        let frame = b.push_struct();
        b.write_int(42);
        b.write_string("hi");
        b.pop_struct(frame);
        assert_eq!(
            b.as_bytes(),
            &[
                0x20, 0x00, 0x00, 0x00, 0x0e, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x04, 0x00,
                0x00, 0x00, 0x2a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00,
                0x08, 0x00, 0x00, 0x00, 0x68, 0x69, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ]
        );

        // 12. Choice Range Int (16, 1, 64)
        let mut b = PodBuilder::new();
        b.write_choice_range_int(16, 1, 64);
        assert_eq!(
            b.as_bytes(),
            &[
                0x1c, 0x00, 0x00, 0x00, 0x13, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00,
                0x01, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ]
        );

        // 13. Choice Flags Int (6)
        let mut b = PodBuilder::new();
        b.write_choice_flags_int(6);
        assert_eq!(
            b.as_bytes(),
            &[
                0x14, 0x00, 0x00, 0x00, 0x13, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
            ]
        );
    }

    #[test]
    fn test_round_trip() {
        let mut b = PodBuilder::new();
        let frame = b.push_struct();
        b.write_none();
        b.write_bool(true);
        b.write_int(12345);
        b.write_long(-9876543210);
        b.write_float(3.125);
        b.write_string("PipeWire Native");
        b.write_fraction(44100, 1);
        b.write_fd(4);
        b.pop_struct(frame);

        let mut parser = PodParser::new(b.as_bytes());
        let val = parser.next().unwrap();
        assert!(parser.is_empty());

        if let PodValue::Struct(items) = val {
            assert_eq!(items.len(), 8);
            assert_eq!(items[0], PodValue::None);
            assert_eq!(items[1], PodValue::Bool(true));
            assert_eq!(items[2], PodValue::Int(12345));
            assert_eq!(items[3], PodValue::Long(-9876543210));
            if let PodValue::Float(f) = items[4] {
                assert!((f - 3.125).abs() < 1e-4);
            } else {
                panic!("expected float");
            }
            assert_eq!(items[5], PodValue::String("PipeWire Native".to_string()));
            assert_eq!(
                items[6],
                PodValue::Fraction {
                    num: 44100,
                    denom: 1
                }
            );
            assert_eq!(items[7], PodValue::Fd(4));
        } else {
            panic!("expected struct");
        }
    }

    #[test]
    fn test_dict_serialization() {
        let mut b = PodBuilder::new();
        let items = [
            ("media.class", "Audio/Sink"),
            ("node.name", "nyx_refrain_airplay"),
        ];
        b.write_dict(&items);

        let mut parser = PodParser::new(b.as_bytes());
        let parsed = parser.next().unwrap();
        assert!(parser.is_empty());

        let dict = PodParser::parse_dict(&parsed).unwrap();
        assert_eq!(dict.len(), 2);
        assert_eq!(
            dict[0],
            ("media.class".to_string(), "Audio/Sink".to_string())
        );
        assert_eq!(
            dict[1],
            ("node.name".to_string(), "nyx_refrain_airplay".to_string())
        );
    }

    #[test]
    fn test_array_id_serialization() {
        let mut b = PodBuilder::new();
        b.write_array_id(&[SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR]);

        let mut parser = PodParser::new(b.as_bytes());
        let parsed = parser.next().unwrap();
        assert!(parser.is_empty());

        if let PodValue::Array { child_type, values } = parsed {
            assert_eq!(child_type, SPA_TYPE_ID);
            assert_eq!(values.len(), 2);
            assert_eq!(values[0], PodValue::Id(SPA_AUDIO_CHANNEL_FL));
            assert_eq!(values[1], PodValue::Id(SPA_AUDIO_CHANNEL_FR));
        } else {
            panic!("expected array");
        }
    }
}
