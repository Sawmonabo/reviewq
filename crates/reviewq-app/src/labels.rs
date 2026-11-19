macro_rules! labels {
    ($name:ident { $($variant:ident => $label:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum $name { $($variant),+ }

        impl std::fmt::Display for $name {
            fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                output.write_str(match self { $(Self::$variant => $label),+ })
            }
        }

        impl std::str::FromStr for $name {
            type Err = $crate::access::AppError;

            fn from_str(label: &str) -> Result<Self, Self::Err> {
                match label {
                    $($label => Ok(Self::$variant),)+
                    _ => Err(Self::Err::InvalidRequest),
                }
            }
        }
    };
}

pub(crate) use labels;
