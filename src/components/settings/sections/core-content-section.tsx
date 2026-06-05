import { useState } from "react"
import { useTranslation } from "react-i18next"
import { Cloud, CloudOff, Loader2 } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import type { SettingsDraft, DraftSetter } from "../settings-types"
import { coreContentStartLogin, coreContentConnectFinish, coreContentSelectFolder, type CoreContentNode } from "@/commands/core-content"

interface Props {
  draft: SettingsDraft
  setDraft: DraftSetter
}

export function CoreContentSection({ draft, setDraft }: Props) {
  const { t } = useTranslation()
  const [connecting, setConnecting] = useState(false)
  const [connectError, setConnectError] = useState<string | null>(null)
  const [folders, setFolders] = useState<CoreContentNode[]>([])
  const [showFolders, setShowFolders] = useState(false)
  const [connected, setConnected] = useState(false)
  const [disconnecting, setDisconnecting] = useState(false)

  // Rehydrate from persisted config
  const [didRehydrate, setDidRehydrate] = useState(false)
  if (!didRehydrate && draft.coreContentEnabled && draft.coreContentBaseUrl && draft.coreContentFolderName) {
    setDidRehydrate(true)
    setConnected(true)
  }

  async function handleConnect() {
    setConnecting(true)
    setConnectError(null)
    try {
      // Open embedded webview for Core Content login.
      // The Rust backend opens a WebviewWindow, injects JS that polls
      // for CCM-XSRF-TOKEN, and returns cookies automatically.
      const loginResult = await coreContentStartLogin(draft.coreContentBaseUrl)

      if (!loginResult.csrfToken) {
        throw new Error("Login did not complete. No CSRF token found.")
      }

      // Validate session and list root folders
      const result = await coreContentConnectFinish(
        draft.coreContentBaseUrl,
        loginResult.csrfToken,
        loginResult.cookiesJson,
      )
      setFolders(result.rootFolders)
      setDraft("coreContentCsrfToken", loginResult.csrfToken)
      setDraft("coreContentCookiesJson", loginResult.cookiesJson)
      setShowFolders(true)
      setConnectError(null)
    } catch (err) {
      setConnectError(String(err))
    } finally {
      setConnecting(false)
    }
  }

  function handleSelectFolder(folder: CoreContentNode) {
    setDraft("coreContentFolderName", folder.name)
    setDraft("coreContentFolderNodeId", folder.id)
    setDraft("coreContentEnabled", true)
    coreContentSelectFolder(folder.id, folder.name).catch((err) =>
      console.error("Failed to select folder:", err)
    )
    setConnected(true)
    setShowFolders(false)
  }

  function handleDisconnect() {
    setDisconnecting(true)
    setDraft("coreContentEnabled", false)
    setDraft("coreContentBaseUrl", "")
    setDraft("coreContentFolderNodeId", "")
    setDraft("coreContentFolderName", "")
    setDraft("coreContentUsername", "")
    setDraft("coreContentPassword", "")
    setDraft("coreContentCsrfToken", "")
    setDraft("coreContentCookiesJson", "")
    setConnected(false)
    setFolders([])
    setShowFolders(false)
    setConnectError(null)
    setDisconnecting(false)
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2">
        {draft.coreContentEnabled && connected ? (
          <Cloud className="h-5 w-5 text-green-500" />
        ) : (
          <CloudOff className="h-5 w-5 text-muted-foreground" />
        )}
        <h2 className="text-lg font-semibold">{t("settings.coreContent.title", "Core Content Connection")}</h2>
      </div>

      {/* Disabled state when xECM is active */}
      {draft.xecmEnabled && (
        <div className="rounded-md border border-amber-200 bg-amber-50 p-3 dark:border-amber-800 dark:bg-amber-950">
          <p className="text-sm text-amber-700 dark:text-amber-300">
            {t("settings.coreContent.disabledByXecm", "Core Content is unavailable while xECM is connected. Disconnect xECM first.")}
          </p>
        </div>
      )}

      {draft.coreContentEnabled && connected ? (
        <div className="space-y-4">
          <div className="rounded-md border border-green-200 bg-green-50 p-4 dark:border-green-800 dark:bg-green-950">
            <p className="text-sm font-medium text-green-700 dark:text-green-300">
              Connected to <strong>{draft.coreContentFolderName}</strong> at {draft.coreContentBaseUrl}
            </p>
          </div>

          <div className="space-y-2">
            <Label>Poll interval (seconds)</Label>
            <Input
              type="number"
              min={10}
              max={300}
              value={draft.coreContentPollIntervalSeconds}
              onChange={(e) =>
                setDraft("coreContentPollIntervalSeconds", parseInt(e.target.value) || 30)
              }
            />
          </div>

          <Button variant="outline" onClick={handleDisconnect} disabled={disconnecting}>
            {disconnecting ? <Loader2 className="mr-2 h-4 w-4 animate-spin" /> : null}
            Disconnect
          </Button>
        </div>
      ) : showFolders ? (
        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">
            Select a folder to use as your source layer:
          </p>
          <div className="space-y-2">
            {folders.map((f) => (
              <button
                key={f.id}
                type="button"
                onClick={() => handleSelectFolder(f)}
                className="w-full rounded-md border px-4 py-3 text-left transition-colors hover:bg-accent hover:text-accent-foreground"
              >
                <div className="font-medium">{f.name}</div>
              </button>
            ))}
          </div>
          <Button variant="ghost" size="sm" onClick={() => setShowFolders(false)}>
            Back
          </Button>
        </div>
      ) : (
        <div className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="cc-url">Base URL</Label>
            <Input
              id="cc-url"
              placeholder="https://corecontent.dev.ca.opentext.com/subscriptions/avstcc"
              value={draft.coreContentBaseUrl}
              onChange={(e) => setDraft("coreContentBaseUrl", e.target.value)}
              disabled={draft.xecmEnabled}
            />
          </div>

          {connectError && (
            <div className="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">
              {connectError}
            </div>
          )}

          <Button
            onClick={handleConnect}
            disabled={connecting || !draft.coreContentBaseUrl || draft.xecmEnabled}
          >
            {connecting ? <Loader2 className="mr-2 h-4 w-4 animate-spin" /> : null}
            Connect to Core Content
          </Button>
        </div>
      )}
    </div>
  )
}
