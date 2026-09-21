const fs = require('fs');
let code = fs.readFileSync('src/App.tsx', 'utf8');
const lines = code.split('\n');
const insertIndex = lines.findIndex(l => l.includes('<footer className="statusbar">'));
const ui = 
      <div style={{padding: "8px", background: "rgba(0,0,0,0.4)", borderRadius: "8px", marginBottom: "10px", display: "flex", gap: "10px", alignItems: "center"}}>
        <button className="primary" style={{backgroundColor: "#b35b14", border: "none", padding: "6px 12px", borderRadius: "4px", color: "white", fontWeight: "bold", cursor: "pointer"}} onClick={async () => {
          try {
            await invoke("enable_afk_mode", { duration: 3600 });
            alert("✅ AFK(무인 공장) 모드가 활성화되었습니다. 모든 에이전트가 Firstmate의 지휘를 받습니다.");
          } catch (e) {
            alert("AFK 모드 설정 실패: " + e);
          }
        }}>💤 /afk (자리비움)</button>
        <button style={{backgroundColor: "#2a3a5c", border: "none", padding: "6px 12px", borderRadius: "4px", color: "white", cursor: "pointer"}} onClick={async () => {
          try {
            alert("1. 모듈 로딩 시작");
            const oauth = await import("oauth-collect");
            const ctrl = {
              onAuth: async (info) => {
                alert("3. 브라우저 열기 시도 중... URL:\\n" + info.url);
                try {
                  const opener = await import("@tauri-apps/plugin-opener");
                  await opener.open(info.url);
                  alert("4. 브라우저 열기 성공!");
                } catch(e) {
                  alert("4. 브라우저 열기 에러: " + e?.message);
                }
              },
              onProgress: (msg) => { console.log("[OAuth]", msg); },
              onAuthorizationResponse: async (state) => {
                const res = window.prompt("5. 로그인이 끝나면 빈 화면이 나올 수 있습니다.\\n해당 브라우저의 전체 주소(URL)를 복사해서 여기에 붙여넣어주세요:");
                return res || "";
              }
            };
            alert("2. Anthropic(Claude) 토큰 획득 엔진 가동!");
            const creds = await oauth.OAUTH_PROVIDERS["anthropic"].login(ctrl);
            alert("6. ✅ Anthropic 토큰 획득 성공!\\nAccess Token: " + creds.access.substring(0, 20) + "...");
          } catch (e) {
            alert("❌ OAuth 로그인 중단/실패: " + e?.message);
          }
        }}>🔑 AI 일괄 로그인 (oauth)</button>
      </div>
;
lines.splice(insertIndex, 0, ui);
fs.writeFileSync('src/App.tsx', lines.join('\n'), 'utf8');
console.log("Success");
