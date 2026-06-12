import { useState } from "react"
import { useTranslation } from "react-i18next"
import { invoke } from "@tauri-apps/api/core"
import { Cloud, CloudOff, Loader2 } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import type { SettingsDraft, DraftSetter } from "../settings-types"

interface Props {
  draft: SettingsDraft
  setDraft: DraftSetter
}

interface XecmWorkspace {
  name: string
  id: number
  type: number
}

export function XecmSection({ draft, setDraft }: Props) {
  const { t } = useTranslation()
  const [password, setPassword] = useState("")
  const [connecting, setConnecting] = useState(false)
  const [connectError, setConnectError] = useState<string | null>(null)
  const [workspaces, setWorkspaces] = useState<XecmWorkspace[]>([])
  const [connected, setConnected] = useState(false)
  const [disconnecting, setDisconnecting] = useState(false)

  // Rehydrate connected state from persisted config on mount
  const [didRehydrate, setDidRehydrate] = useState(false)
  if (!didRehydrate && draft.xecmEnabled && draft.xecmBaseUrl && draft.xecmWorkspaceName) {
    setDidRehydrate(true)
    setConnected(true)
  }

  async function handleConnect() {
    setConnecting(true)
    setConnectError(null)
    try {
      const result = await invoke<{ ticket: string; workspaces: XecmWorkspace[] }>("xecm_connect", {
        baseUrl: draft.xecmBaseUrl,
        username: draft.xecmUsername,
        password,
      })
      setWorkspaces(result.workspaces)
      setDraft("xecmTicket", result.ticket)
      setDraft("xecmPassword", password)
      setConnected(true)
      setConnectError(null)
    } catch (err) {
      setConnectError(String(err))
      setConnected(false)
    } finally {
      setConnecting(false)
    }
  }

  function handleSelectWorkspace(ws: XecmWorkspace) {
    setDraft("xecmWorkspaceName", ws.name)
    setDraft("xecmWorkspaceNodeId", ws.id)
    setDraft("xecmEnabled", true)
  }

  function handleDisconnect() {
    setDisconnecting(true)
    setDraft("xecmEnabled", false)
    setDraft("xecmBaseUrl", "")
    setDraft("xecmWorkspaceName", "")
    setDraft("xecmWorkspaceNodeId", 0)
    setDraft("xecmUsername", "")
    setDraft("xecmPassword", "")
    setDraft("xecmTicket", "")
    setConnected(false)
    setWorkspaces([])
    setPassword("")
    setConnectError(null)
    setDisconnecting(false)
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2">
        {draft.xecmEnabled && connected ? (
          <Cloud className="h-5 w-5 text-green-500" />
        ) : (
          <CloudOff className="h-5 w-5 text-muted-foreground" />
        )}
        <h2 className="text-lg font-semibold">{t("settings.xecm.title", "OpenText Integration")}</h2>
      </div>

      {draft.coreContentEnabled && (
        <div className="rounded-md border border-amber-200 bg-amber-50 p-3 dark:border-amber-800 dark:bg-amber-950">
          <p className="text-sm text-amber-700 dark:text-amber-300">
            xECM is unavailable while Core Content is connected. Disconnect Core Content first.
          </p>
        </div>
      )}

      <div className={draft.coreContentEnabled ? "opacity-50 pointer-events-none" : ""}>
        {draft.xecmEnabled && connected ? (
        <div className="space-y-4">
          <div className="rounded-md border border-green-200 bg-green-50 p-4 dark:border-green-800 dark:bg-green-950">
            <p className="text-sm font-medium text-green-700 dark:text-green-300">
              {t("settings.xecm.connectedTo", {
                defaultValue: "Connected to {{workspace}} at {{url}}",
                workspace: draft.xecmWorkspaceName,
                url: draft.xecmBaseUrl,
              })}
            </p>
          </div>

          <div className="space-y-2">
            <Label>{t("settings.xecm.pollInterval", "Poll interval (seconds)")}</Label>
            <Input
              type="number"
              min={10}
              max={300}
              value={draft.xecmPollIntervalSeconds}
              onChange={(e) =>
                setDraft("xecmPollIntervalSeconds", parseInt(e.target.value) || 30)
              }
            />
            <p className="text-xs text-muted-foreground">
              {t("settings.xecm.pollIntervalHint", "How often to check xECM for file changes. Minimum 10 seconds.")}
            </p>
          </div>

          <Button variant="outline" onClick={handleDisconnect} disabled={disconnecting}>
            {disconnecting ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : null}
            {t("settings.xecm.disconnect", "Disconnect")}
          </Button>
        </div>
      ) : connected && workspaces.length > 0 ? (
        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">
            {t("settings.xecm.selectWorkspace", "Select a workspace to use as your source layer:")}
          </p>
          <div className="space-y-2">
            {workspaces.map((ws) => (
              <button
                key={ws.id}
                type="button"
                onClick={() => handleSelectWorkspace(ws)}
                className="w-full rounded-md border px-4 py-3 text-left transition-colors hover:bg-accent hover:text-accent-foreground"
              >
                <div className="font-medium">{ws.name}</div>
                <div className="text-xs text-muted-foreground">
                  {t("settings.xecm.workspaceType", "Type {{type}}", { type: ws.type })}
                  {" · "}
                  {t("settings.xecm.nodeId", "ID {{id}}", { id: ws.id })}
                </div>
              </button>
            ))}
          </div>
          <Button variant="ghost" size="sm" onClick={handleDisconnect}>
            {t("settings.xecm.goBack", "Back")}
          </Button>
        </div>
      ) : (
        <div className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="xecm-url">{t("settings.xecm.baseUrl", "Base URL")}</Label>
            <Input
              id="xecm-url"
              placeholder="http://192.168.0.29/otcs/cs.exe/api/v1"
              value={draft.xecmBaseUrl}
              onChange={(e) => setDraft("xecmBaseUrl", e.target.value)}
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="xecm-username">{t("settings.xecm.username", "Username")}</Label>
            <Input
              id="xecm-username"
              value={draft.xecmUsername}
              onChange={(e) => setDraft("xecmUsername", e.target.value)}
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="xecm-password">{t("settings.xecm.password", "Password")}</Label>
            <Input
              id="xecm-password"
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
          </div>

          {connectError && (
            <div className="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">
              {connectError}
            </div>
          )}

          <Button onClick={handleConnect} disabled={connecting || !draft.xecmBaseUrl || !draft.xecmUsername || !password}>
            {connecting ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : null}
            {t("settings.xecm.connect", "Connect")}
          </Button>
        </div>
      )}
      </div>
    </div>
  )
}
