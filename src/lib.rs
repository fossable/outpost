use anyhow::Result;

#[derive(Debug, Clone)]
pub struct PortMapping {
    pub local: u16,
    pub public: u16,
}

impl PortMapping {
    pub fn from_vec(p: Vec<String>) -> Result<Vec<Self>> {
        let mut ports = Vec::new();

        for mapping in p.into_iter() {
            let mapping = mapping.try_into()?;

            // Check for duplicates
            // TODO

            ports.push(mapping);
        }

        Ok(ports)
    }
}

impl TryFrom<String> for PortMapping {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        if let Some((local, public)) = value.split_once(":") {
            Ok(PortMapping {
                local: local.parse::<u16>()?,
                public: public.parse::<u16>()?,
            })
        } else {
            Ok(PortMapping {
                local: value.parse::<u16>()?,
                public: value.parse::<u16>()?,
            })
        }
    }
}
