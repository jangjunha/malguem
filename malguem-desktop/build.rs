fn main() {
    #[cfg(target_os = "windows")]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("icon.ico")
            .set("ProductName", "Malguem")
            .set("FileDescription", "Malguem - P2P Voice Chat Application")
            .set("CompanyName", "heek.kr")
            .set("LegalCopyright", "Copyright © heek.kr")
            .set("OriginalFilename", "malguem-desktop.exe")
            .set("InternalName", "malguem-desktop");
        res.compile()?;
    }
}
