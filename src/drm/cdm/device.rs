//! # Widevine devices
//!
//! The widevine crate supports the custom *.wvd serialization format from the pywidevine library,
//! so you can import

use std::collections::BTreeMap;

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::{FromPrimitive, ToPrimitive};
use protobuf::Message;
use rsa::{
    RsaPrivateKey,
    pkcs1::{DecodeRsaPrivateKey, EncodeRsaPrivateKey},
};

use super::Error;
use widevine_proto::license_protocol::{
    ClientIdentification, client_identification::ClientCapabilities as PbClientCapabilities,
    client_identification::client_capabilities,
};

/// Widevine device
#[derive(Debug, Clone)]
pub struct Device {
    pub(crate) device_type: DeviceType,
    pub(crate) security_level: SecurityLevel,
    pub(crate) private_key: RsaPrivateKey,
    pub(crate) client_id: ClientIdentification,
}

/// Metadata of a Widevine device, containing its DRM capabilities and device information
#[derive(Debug, Clone)]
pub struct ClientMetadata {
    /// Supported DRM capabilities of the device
    pub capabilities: ClientCapabilities,
    /// Device information
    ///
    /// Common keys:
    /// - `device_id`
    /// - `device_name`
    /// - `model_name`
    /// - `product_name`
    /// - `architecture_name`
    /// - `company_name`
    /// - `widevine_cdm_version`
    /// - `build_info`
    pub client_info: BTreeMap<String, String>,
    /// Widevine device info
    pub wvd: ClientMetadataWvd,
}

/// Supported DRM capabilities of the Widevine device
#[derive(Default, Debug, Clone)]
#[allow(missing_docs)]
pub struct ClientCapabilities {
    pub client_token: bool,
    pub session_token: bool,
    pub video_resolution_constraints: bool,
    pub max_hdcp_version: HdcpVersion,
    pub oem_crypto_api_version: Option<u32>,
    pub anti_rollback_usage_table: bool,
    pub srm_version: Option<u32>,
    pub can_update_srm: bool,
    pub supported_certificate_key_type: Vec<CertificateKeyType>,
    pub analog_output_capabilities: AnalogOutputCapabilities,
    pub can_disable_analog_output: bool,
    pub resource_rating_tier: Option<u32>,
}

/// Device's supported [HDCP](https://de.wikipedia.org/wiki/High-bandwidth_Digital_Content_Protection) version
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum HdcpVersion {
    #[default]
    HDCP_NONE = 0,
    HDCP_V1 = 1,
    HDCP_V2 = 2,
    HDCP_V2_1 = 3,
    HDCP_V2_2 = 4,
    HDCP_V2_3 = 5,
    HDCP_NO_DIGITAL_OUTPUT = 255,
}

/// Widevine certificate key type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum CertificateKeyType {
    RSA_2048 = 0,
    RSA_3072 = 1,
    ECC_SECP256R1 = 2,
    ECC_SECP384R1 = 3,
    ECC_SECP521R1 = 4,
}

/// Device's analog output capabilities
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum AnalogOutputCapabilities {
    #[default]
    ANALOG_OUTPUT_UNKNOWN = 0,
    ANALOG_OUTPUT_NONE = 1,
    ANALOG_OUTPUT_SUPPORTED = 2,
    ANALOG_OUTPUT_SUPPORTS_CGMS_A = 3,
}

impl From<client_capabilities::HdcpVersion> for HdcpVersion {
    fn from(value: client_capabilities::HdcpVersion) -> Self {
        match value {
            client_capabilities::HdcpVersion::HDCP_NONE => Self::HDCP_NONE,
            client_capabilities::HdcpVersion::HDCP_V1 => Self::HDCP_V1,
            client_capabilities::HdcpVersion::HDCP_V2 => Self::HDCP_V2,
            client_capabilities::HdcpVersion::HDCP_V2_1 => Self::HDCP_V2_1,
            client_capabilities::HdcpVersion::HDCP_V2_2 => Self::HDCP_V2_2,
            client_capabilities::HdcpVersion::HDCP_V2_3 => Self::HDCP_V2_3,
            client_capabilities::HdcpVersion::HDCP_NO_DIGITAL_OUTPUT => {
                Self::HDCP_NO_DIGITAL_OUTPUT
            }
        }
    }
}

impl From<client_capabilities::CertificateKeyType> for CertificateKeyType {
    fn from(value: client_capabilities::CertificateKeyType) -> Self {
        match value {
            client_capabilities::CertificateKeyType::RSA_2048 => Self::RSA_2048,
            client_capabilities::CertificateKeyType::RSA_3072 => Self::RSA_3072,
            client_capabilities::CertificateKeyType::ECC_SECP256R1 => Self::ECC_SECP256R1,
            client_capabilities::CertificateKeyType::ECC_SECP384R1 => Self::ECC_SECP384R1,
            client_capabilities::CertificateKeyType::ECC_SECP521R1 => Self::ECC_SECP521R1,
        }
    }
}

impl From<client_capabilities::AnalogOutputCapabilities> for AnalogOutputCapabilities {
    fn from(value: client_capabilities::AnalogOutputCapabilities) -> Self {
        match value {
            client_capabilities::AnalogOutputCapabilities::ANALOG_OUTPUT_UNKNOWN => {
                Self::ANALOG_OUTPUT_UNKNOWN
            }
            client_capabilities::AnalogOutputCapabilities::ANALOG_OUTPUT_NONE => {
                Self::ANALOG_OUTPUT_NONE
            }
            client_capabilities::AnalogOutputCapabilities::ANALOG_OUTPUT_SUPPORTED => {
                Self::ANALOG_OUTPUT_SUPPORTED
            }
            client_capabilities::AnalogOutputCapabilities::ANALOG_OUTPUT_SUPPORTS_CGMS_A => {
                Self::ANALOG_OUTPUT_SUPPORTS_CGMS_A
            }
        }
    }
}

/// Widevine device info
#[derive(Debug, Clone)]
pub struct ClientMetadataWvd {
    /// Widevine device type
    pub device_type: DeviceType,
    /// Widevine security level (1-3)
    pub security_level: SecurityLevel,
}

impl From<PbClientCapabilities> for ClientCapabilities {
    fn from(value: PbClientCapabilities) -> Self {
        Self {
            client_token: value.client_token(),
            session_token: value.session_token(),
            video_resolution_constraints: value.video_resolution_constraints(),
            max_hdcp_version: value.max_hdcp_version().into(),
            oem_crypto_api_version: value.oem_crypto_api_version,
            anti_rollback_usage_table: value.anti_rollback_usage_table(),
            srm_version: value.srm_version,
            can_update_srm: value.can_update_srm(),
            supported_certificate_key_type: value
                .supported_certificate_key_type
                .iter()
                .filter_map(|t| t.enum_value().ok().map(CertificateKeyType::from))
                .collect(),
            analog_output_capabilities: value.analog_output_capabilities().into(),
            can_disable_analog_output: value.can_disable_analog_output(),
            resource_rating_tier: value.resource_rating_tier,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, FromPrimitive, ToPrimitive)]
enum WvdVersion {
    V1 = 1,
    V2,
}

/// Widevine device type
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromPrimitive, ToPrimitive)]
pub enum DeviceType {
    /// CDM extracted from Google Chrome
    CHROME = 1,
    /// CDM extracted from an Android device
    ANDROID,
}

/// Security level of a Widevine device
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromPrimitive, ToPrimitive)]
pub enum SecurityLevel {
    /// **L1** Highest protection, no limitation for quality or resolution.
    /// Cryptography and media processing have to be executed in a TEE.
    L1 = 1,
    /// **L2** Only cryptographic operatione are executed in a TEE
    L2,
    /// **L3** Only software-based protection without a TEE
    L3,
}

impl Device {
    /// Create a new Widevine device
    pub fn new(
        device_type: DeviceType,
        security_level: SecurityLevel,
        private_key: RsaPrivateKey,
        client_id: &[u8],
    ) -> Result<Self, Error> {
        let client_id = ClientIdentification::parse_from_bytes(client_id)?;

        Ok(Self {
            device_type,
            security_level,
            private_key,
            client_id,
        })
    }

    /// Read a Widevine device from a `.wvd` file, as used by [pywidevine](https://pypi.org/project/pywidevine/).
    pub fn read_wvd<R: std::io::Read>(mut reader: R) -> Result<Self, Error> {
        let mut magic = [0; 3];
        reader.read_exact(&mut magic)?;
        if &magic != b"WVD" {
            return Err(Error::InvalidInput("expected `WVD` magic".into()));
        }

        WvdVersion::from_u8(reader.read_u8()?)
            .ok_or(Error::InvalidInput("invalid version".into()))?;

        let device_type = DeviceType::from_u8(reader.read_u8()?)
            .ok_or(Error::InvalidInput("invalid device type".into()))?;

        let security_level = SecurityLevel::from_u8(reader.read_u8()?)
            .ok_or(Error::InvalidInput("invalid security level".into()))?;

        let fzero = reader.read_u8()?;
        if fzero != 0 {
            return Err(Error::InvalidInput("invalid flag padding".into()));
        }

        // There may be flag padding which has to be skipped
        let mut private_key_len;
        loop {
            private_key_len = reader.read_u16::<BigEndian>()?;
            if private_key_len == 0 {
                for _ in 0..5 {
                    reader.read_u8()?;
                }
            } else {
                break;
            }
        }
        let mut private_key_bts: Vec<u8> = vec![0; private_key_len.into()];
        reader.read_exact(&mut private_key_bts)?;
        let private_key = RsaPrivateKey::from_pkcs1_der(&private_key_bts)
            .map_err(|e| Error::InvalidInput(format!("invalid private key: {e}").into()))?;

        let client_id_len = reader.read_u16::<BigEndian>()?;
        let mut client_id: Vec<u8> = vec![0; client_id_len.into()];
        reader.read_exact(&mut client_id)?;

        Self::new(device_type, security_level, private_key, &client_id)
    }

    /// Write a Widevine device to a `.wvd` file, as used by [pywidevine](https://pypi.org/project/pywidevine/).
    pub fn write_wvd<W: std::io::Write>(&self, mut writer: W) -> Result<(), Error> {
        writer.write_all(b"WVD")?; // magic
        writer.write_u8(2)?; // version
        writer.write_u8(self.device_type.to_u8().unwrap())?;
        writer.write_u8(self.security_level.to_u8().unwrap())?;
        writer.write_u8(0)?; // flag padding

        let private_key_bts = self
            .private_key
            .to_pkcs1_der()
            .map_err(|e| Error::InvalidInput(format!("rsa private key: {e}").into()))?;
        writer.write_u16::<BigEndian>(
            usize::try_from(private_key_bts.len())
                .ok()
                .and_then(|v| u16::try_from(v).ok())
                .ok_or(Error::InvalidInput("rsa private_key_len overflow".into()))?,
        )?;
        writer.write_all(private_key_bts.as_bytes())?;

        let client_id_bts = self.client_id.write_to_bytes()?;
        writer.write_u16::<BigEndian>(
            u16::try_from(client_id_bts.len())
                .map_err(|_| Error::InvalidInput("client_id_len overflow".into()))?,
        )?;
        writer.write_all(&client_id_bts)?;
        Ok(())
    }

    /// Get the device type (Chrome/Android) of the Widevine device.
    pub fn device_type(&self) -> DeviceType {
        self.device_type
    }

    /// Get the security level (L1–L3) of the Widevine device.
    pub fn security_level(&self) -> SecurityLevel {
        self.security_level
    }

    /// Get the full metadata of the Widevine device
    pub fn metadata(&self) -> ClientMetadata {
        ClientMetadata {
            capabilities: self
                .client_id
                .client_capabilities
                .as_ref()
                .map(|cap| ClientCapabilities::from(cap.clone()))
                .unwrap_or_default(),
            client_info: self
                .client_id
                .client_info
                .iter()
                .map(|itm| (itm.name().to_owned(), itm.value().to_owned()))
                .collect(),
            wvd: ClientMetadataWvd {
                device_type: self.device_type,
                security_level: self.security_level,
            },
        }
    }
}
