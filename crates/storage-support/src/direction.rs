use anyhow::bail;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ClientToServer = 0,
    ServerToClient = 1,
}

impl Direction {
    pub fn from_u8(value: u8) -> anyhow::Result<Self> {
        match value {
            0 => Ok(Self::ClientToServer),
            1 => Ok(Self::ServerToClient),
            _ => bail!("invalid tlc direction {value}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Direction;

    #[test]
    fn direction_round_trips_wire_values() {
        assert_eq!(Direction::ClientToServer as u8, 0);
        assert_eq!(Direction::ServerToClient as u8, 1);
        assert_eq!(Direction::from_u8(0).unwrap(), Direction::ClientToServer);
        assert_eq!(Direction::from_u8(1).unwrap(), Direction::ServerToClient);
        assert!(Direction::from_u8(9).is_err());
    }
}
