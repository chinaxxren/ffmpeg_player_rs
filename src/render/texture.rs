pub fn create_yuv_texture(&mut self, width: i32, height: i32) -> Result<(), String> {
    self.texture = Some(self.canvas.create_texture_streaming(
        SDL_PixelFormat::SDL_PIXELFORMAT_IYUV,  // 确保使用正确的YUV格式
        width,
        height
    )?);
    
    self.width = width;
    self.height = height;
    Ok(())
}

pub fn update_yuv_texture(&mut self, y_plane: &[u8], u_plane: &[u8], v_plane: &[u8], y_pitch: usize) -> Result<(), String> {
    if let Some(texture) = &mut self.texture {
        texture.update_yuv(
            None,                // 整个纹理
            y_plane,            // Y平面数据
            y_pitch as i32,     // Y平面pitch
            u_plane,            // U平面数据
            (y_pitch / 2) as i32, // U平面pitch (注意是Y的一半)
            v_plane,            // V平面数据
            (y_pitch / 2) as i32  // V平面pitch (注意是Y的一半)
        )?;
    }
    Ok(())
} 