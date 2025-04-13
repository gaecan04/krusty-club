/*use bincode::{config, Decode, Encode};
//use common::web_messages::{Serializable, SerializationError};

// Wrapper struct that will implement our own serialization trait
pub struct SerializableWrapper<T>(pub T);

// Our own trait that doesn't depend on the problematic implementation
pub trait FixedSerializable {
    fn serialize(&self) -> Result<Vec<u8>, SerializationError>;
    fn deserialize(data: Vec<u8>) -> Result<Self, SerializationError> where Self: Sized;
}

// Implement our trait for the wrapper
impl<T> FixedSerializable for SerializableWrapper<T>
where
    T: Encode + Decode<bincode::config::Configuration>
{
    fn serialize(&self) -> Result<Vec<u8>, SerializationError> {
        bincode::encode_to_vec(&self.0, config::standard()).map_err(|_| SerializationError)
    }

    fn deserialize(data: Vec<u8>) -> Result<Self, SerializationError> {
        match bincode::decode_from_slice::<T, _>(&data, config::standard()) {
            Ok((value, _)) => Ok(SerializableWrapper(value)),
            Err(_) => Err(SerializationError),
        }
    }
}

// Convenience methods to unwrap/wrap
impl<T> SerializableWrapper<T> {
    pub fn new(value: T) -> Self {
        SerializableWrapper(value)
    }

    pub fn into_inner(self) -> T {
        self.0
    }
}
*/