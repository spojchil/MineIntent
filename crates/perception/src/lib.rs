//! 感知：按需看画面的 `view` 工具。Free 类——看不占身体域、不受屏压制。
//!
//! 薄壳：采集与成像在组合根注入的 [`PictureDoor`] 后面，这里只做参数校验与
//! 把图片作为原生图片回执交回。

mod picture;

pub use picture::{Picture, PictureDoor, PictureTools, Region};
