import { invoke } from "@tauri-apps/api/core"

export interface CoreContentNode {
  id: string
  name: string
}

export interface CoreContentConnectResult {
  rootFolders: CoreContentNode[]
}

export interface CoreContentLoginResult {
  csrfToken: string
  cookiesJson: string
  /** JSON array of CoreContentNode objects fetched by the webview JS. */
  rootFoldersJson: string
}

export function setCoreContentConfig(config: unknown): Promise<string> {
  return invoke<string>("set_core_content_config", { config })
}

export function coreContentStartLogin(
  baseUrl: string,
): Promise<CoreContentLoginResult> {
  return invoke<CoreContentLoginResult>("core_content_start_login", { baseUrl })
}

export function coreContentConnectFinish(
  baseUrl: string,
  csrfToken: string,
  cookiesJson: string,
): Promise<CoreContentConnectResult> {
  return invoke<CoreContentConnectResult>("core_content_connect_finish", {
    baseUrl,
    csrfToken,
    cookiesJson,
  })
}

export function coreContentSelectFolder(
  folderNodeId: string,
  folderName: string,
): Promise<string> {
  return invoke<string>("core_content_select_folder", {
    folderNodeId,
    folderName,
  })
}
