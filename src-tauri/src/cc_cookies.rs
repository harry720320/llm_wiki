/// Extract ALL cookies (including HttpOnly) from the WebView2 cookie store.
/// The GetCookies callback sends directly to the outer channel — no blocking
/// inside with_webview, which would deadlock the main thread's message pump
/// and prevent the callback from ever firing.
#[cfg(target_os = "windows")]
pub fn extract_all_cookies(
    webview: &tauri::WebviewWindow,
    uri: &str,
) -> Result<std::collections::HashMap<String, String>, String> {
    use std::sync::mpsc;
    use webview2_com::GetCookiesCompletedHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_13, ICoreWebView2CookieManager, ICoreWebView2Profile5,
    };
    use windows_core::Interface;

    let (tx, rx) = mpsc::channel();
    let tx_err = tx.clone();
    let uri = uri.to_string();

    webview
        .with_webview(move |platform| {
            let _ = (|| -> Result<(), String> {
                let controller = platform.controller();
                let core = unsafe { controller.CoreWebView2() }
                    .map_err(|e| format!("CoreWebView2: {e}"))?;
                let core13: ICoreWebView2_13 = core
                    .cast()
                    .map_err(|e| format!("cast to _13: {e}"))?;
                let profile = unsafe { core13.Profile() }
                    .map_err(|e| format!("Profile: {e}"))?;
                let profile5: ICoreWebView2Profile5 = profile
                    .cast()
                    .map_err(|e| format!("cast to Profile5: {e}"))?;
                let manager: ICoreWebView2CookieManager = unsafe { profile5.CookieManager() }
                    .map_err(|e| format!("CookieManager: {e}"))?;

                let handler = GetCookiesCompletedHandler::create(Box::new(
                    move |_errorcode, cookie_list| {
                        let mut map = std::collections::HashMap::new();
                        if let Some(list) = cookie_list {
                            let mut count: u32 = 0;
                            if unsafe { list.Count(&mut count) }.is_ok() {
                                for i in 0..count {
                                    if let Ok(cookie) = unsafe { list.GetValueAtIndex(i) } {
                                        let mut name = windows_core::PWSTR::null();
                                        let mut value = windows_core::PWSTR::null();
                                        if unsafe { cookie.Name(&mut name) }.is_ok()
                                            && unsafe { cookie.Value(&mut value) }.is_ok()
                                        {
                                            if let (Ok(n), Ok(v)) = (
                                                unsafe { name.to_string() },
                                                unsafe { value.to_string() },
                                            ) {
                                                map.insert(n, v);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        let _ = tx.send(Ok(map));
                        Ok(())
                    },
                ));

                unsafe {
                    manager.GetCookies(&windows_core::HSTRING::from(&uri), &handler)
                        .map_err(|e| format!("GetCookies: {e}"))?;
                }
                Ok(())
            })()
            .map_err(|e| {
                let _ = tx_err.send(Err(e));
            });
        })
        .map_err(|e| format!("with_webview: {e}"))?;

    rx.recv_timeout(std::time::Duration::from_secs(15))
        .map_err(|_| "timeout waiting for GetCookies callback".to_string())?
}

#[cfg(not(target_os = "windows"))]
pub fn extract_all_cookies(
    _webview: &tauri::WebviewWindow,
    _uri: &str,
) -> Result<std::collections::HashMap<String, String>, String> {
    Ok(std::collections::HashMap::new())
}
