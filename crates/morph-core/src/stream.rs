use futures::stream::BoxStream;

use crate::error::GatewayError;
use crate::request::ResponseEvent;

/// Every provider call, streaming or not, produces one of these. See the
/// doc comment on `ResponseEvent` for why this uniform shape matters.
pub type ResponseStream = BoxStream<'static, Result<ResponseEvent, GatewayError>>;
