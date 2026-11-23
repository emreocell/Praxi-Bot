import { useMemo } from "react";
import { MOCK_BASE_URL } from "../api/mockMode";
import { type HomeClient, type LocalProviders, botsHome } from "../lib/botsHome";
import { canManageProviders } from "../lib/providerConnection";
import { addProvider, setDefaultProvider } from "../lib/providers";
import { useDaemon } from "./DaemonContext";
import { useActiveHost } from "./useActiveHost";

/**
 * `botsHome` wired to the running app: writes go to the bots' home (the adopted host, through the
 * paired channel) or to this computer. The host authorises these writes by the paired desktop's
 * identity (host administration); this computer's owner key is NOT sent to it - that key only
 * guards the local copy-setup call.
 */
export function useBotsHome() {
  const { client, localApi, localConn } = useDaemon();
  const active = useActiveHost();
  return useMemo(() => {
    const mock = client?.baseUrl === MOCK_BASE_URL;
    // While a server is the active computer `client` is that server's client: this computer is a
    // paired peer there, so keys and the default model go through host administration.
    const host: HomeClient | null = active.isHost && client ? client : null;
    const local = mock ? client : localApi;
    const target = { connection: localConn, baseUrl: localApi?.baseUrl };
    const fail = (r: { code: number; stdout: string; stderr: string }) => {
      if (r.code !== 0) {
        throw new Error((r.stderr || r.stdout).trim() || `provider setup failed (${r.code})`);
      }
    };
    const localProviders: LocalProviders = {
      async saveProvider(id, p) {
        if (mock) {
          if (p.keyEnv && p.apiKey) {
            await client?.setSecret(p.keyEnv, p.apiKey);
          }
          return;
        }
        if (!canManageProviders(target)) {
          throw new Error("providers_require_app_daemon");
        }
        fail(await addProvider({ id, baseUrl: p.baseUrl, model: p.model, apiKey: p.apiKey }, target));
        await local?.reloadProviders();
      },
      async setDefault(id, model) {
        if (mock) {
          return;
        }
        fail(await setDefaultProvider(id, target, model));
        await local?.reloadProviders();
      }
    };
    const writer = local ?? client;
    return botsHome({
      local: writer ?? { createBot: reject, updateBot: reject },
      localProviders,
      host
    });
  }, [client, localApi, localConn, active.isHost]);
}

const reject = () => Promise.reject(new Error("local_daemon_unavailable"));
