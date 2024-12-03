pub fn render(&mut self) -> Result<(), String> {
    self.canvas.clear();
    
    if let Some(texture) = &self.texture {
        // 设置混合模式
        self.canvas.set_blend_mode(SDL_BlendMode::SDL_BLENDMODE_NONE);
        // 设置颜色调制
        texture.set_color_mod(255, 255, 255);
        // 渲染纹理
        self.canvas.copy(texture, None, None)?;
    }
    
    self.canvas.present();
    Ok(())
} 