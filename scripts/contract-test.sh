#!/usr/bin/env bash
# Wire-compatibility contract test for cap-server.
# Simulates the exact requests Cap Desktop makes, using the shapes from
# packages/web-api-contract/src/desktop.ts and apps/desktop/src-tauri/src/api.rs.
set -euo pipefail

BASE="${BASE:-http://localhost:8080}"
PASS=0
FAIL=0

json_get() {
  python3 -c "import sys,json
try:
    d=json.load(sys.stdin)
except Exception as e:
    print('')
    sys.exit()
v=d['$1']
if isinstance(v,bool):
    print(str(v).lower())
else:
    print(v)" 2>/dev/null
}

check() {
  local name="$1" expected="$2" actual="$3"
  if [[ "$actual" == "$expected" ]]; then
    PASS=$((PASS+1))
    echo "  ok: $name"
  else
    FAIL=$((FAIL+1))
    echo "  FAIL: $name (expected $expected, got $actual)"
  fi
}

echo "== auth =="

# GET session/request should serve the login/register page
CODE=$(curl -s -o /tmp/body -w "%{http_code}" "$BASE/api/desktop/session/request?type=api_key")
check "session/request GET" "200" "$CODE"
grep -q "Connect Cap Desktop" /tmp/body && check "session page content" "found" "found" || check "session page content" "found" "missing"

# Register a user via the desktop session form -> redirect with api_key
LOCATION=$(curl -s -D - -o /dev/null -X POST "$BASE/api/desktop/session/request" \
  -d "action=register&username=contract-test&password=test1234&port=9999" | tr -d '\r' | grep -i '^location:' | sed 's/^[Ll]ocation: //')
echo "  location: $LOCATION"
case "$LOCATION" in
  "http://127.0.0.1:9999/?type=api_key&api_key="*) check "register redirect" "loopback+type" "loopback+type" ;;
  *) check "register redirect" "loopback+type" "$LOCATION" ;;
esac
API_KEY=$(echo "$LOCATION" | sed 's/.*api_key=//;s/&.*//')
USER_ID=$(echo "$LOCATION" | sed 's/.*user_id=//')
echo "  user_id: $USER_ID"

# /api/auth/register JSON endpoint
RESP=$(curl -s -X POST -H "Content-Type: application/json" \
  -d '{"username":"json-test","password":"jsonpw1234"}' \
  "$BASE/api/auth/register")
check "json register has token" "token" "$(echo "$RESP" | json_get token 2>/dev/null || echo "missing")"
JSON_TOKEN=$(echo "$RESP" | json_get token)

# /api/auth/login JSON endpoint
RESP=$(curl -s -X POST -H "Content-Type: application/json" \
  -d '{"username":"contract-test","password":"test1234"}' \
  "$BASE/api/auth/login")
check "json login has token" "token" "$(echo "$RESP" | json_get token 2>/dev/null || echo "missing")"

# Invalid credentials rejected
CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST -H "Content-Type: application/json" \
  -d '{"username":"contract-test","password":"wrongpass"}' \
  "$BASE/api/auth/login")
check "bad login" "401" "$CODE"

# Duplicate username rejected
CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST -H "Content-Type: application/json" \
  -d '{"username":"contract-test","password":"test1234"}' \
  "$BASE/api/auth/register")
check "duplicate register" "400" "$CODE"

echo ""
echo "== desktop endpoints =="

RESP=$(curl -s -H "Authorization: Bearer $API_KEY" "$BASE/api/desktop/user/profile")
[[ "$RESP" == *'"name"'* ]] && check "profile" "json" "json" || check "profile" "json" "$RESP"
[[ "$RESP" == *'"username"'* ]] && check "profile has username" "json" "json" || check "profile has username" "json" "missing"

RESP=$(curl -s -H "Authorization: Bearer $API_KEY" "$BASE/api/desktop/plan")
check "plan.upgraded" "true" "$(echo "$RESP" | json_get upgraded)"
check "plan.stripe" "active" "$(echo "$RESP" | json_get stripeSubscriptionStatus)"

RESP=$(curl -s -H "Authorization: Bearer $API_KEY" "$BASE/api/desktop/organizations")
check "organizations" "[]" "$RESP"

RESP=$(curl -s -H "Authorization: Bearer $API_KEY" "$BASE/api/desktop/s3/config/get")
check "s3 config source" "default" "$(echo "$RESP" | json_get source)"

RESP=$(curl -s -H "Authorization: Bearer $API_KEY" "$BASE/api/desktop/storage/integrations")
check "storage activeProvider" "s3" "$(echo "$RESP" | json_get activeProvider)"

# Auth required: wrong key => 401
CODE=$(curl -s -o /dev/null -w "%{http_code}" -H "Authorization: Bearer bad-key" "$BASE/api/desktop/plan")
check "bad auth" "401" "$CODE"

# JWT token also works for API endpoints
CODE=$(curl -s -o /dev/null -w "%{http_code}" -H "Authorization: Bearer $JSON_TOKEN" "$BASE/api/desktop/plan")
check "jwt auth works" "200" "$CODE"

echo ""
echo "== video create =="

RESP=$(curl -s -H "Authorization: Bearer $API_KEY" "$BASE/api/desktop/video/create?recordingMode=desktopMP4&name=contract-test")
VIDEO_ID=$(echo "$RESP" | json_get id)
check "video create id" "1" "$(echo -n "$VIDEO_ID" | grep -cE '^[0-9a-f]{8}-' | tr -d ' ')"
# user_id should be the registered user's uuid, not the legacy u_single_user
[[ "$USER_ID" == "$(echo "$RESP" | json_get user_id)" ]] && check "video create user_id" "matches" "matches" || check "video create user_id" "matches" "mismatch"

echo ""
echo "== single-part upload (signed) =="

RESP=$(curl -s -H "Authorization: Bearer $API_KEY" -H "Content-Type: application/json" \
  -d "{\"videoId\":\"$VIDEO_ID\",\"subpath\":\"result.mp4\",\"method\":\"put\"}" \
  "$BASE/api/upload/signed")
check "signed has presignedPutData" "presignedPutData" "$(echo "$RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); print(list(d.keys())[0] if 'presignedPutData' in d else '')" 2>/dev/null)"
UPLOAD_URL=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['presignedPutData']['url'])" 2>/dev/null)

# Upload a small real mp4
ffmpeg -y -f lavfi -i testsrc=duration=1:size=160x120:rate=10 -c:v libx264 -preset ultrafast -pix_fmt yuv420p /tmp/contract.mp4 >/dev/null 2>&1
CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST -T /tmp/contract.mp4 "$UPLOAD_URL")
check "PUT upload" "200" "$CODE"

RESP=$(curl -s -H "Authorization: Bearer $API_KEY" -H "Content-Type: application/json" \
  -d "{\"videoId\":\"$VIDEO_ID\",\"uploaded\":100,\"total\":100,\"updatedAt\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}" \
  "$BASE/api/desktop/video/progress")
check "progress returns true" "true" "$RESP"

echo ""
echo "== playback =="

CODE=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/s/$VIDEO_ID")
check "share page" "200" "$CODE"

CODE=$(curl -s -o /dev/null -w "%{http_code}" -L "$BASE/api/playlist?videoId=$VIDEO_ID&videoType=mp4")
check "playlist mp4" "200" "$CODE"

MEDIA_URL=$(curl -s -I "$BASE/api/playlist?videoId=$VIDEO_ID&videoType=mp4" | tr -d '\r' | grep -i '^location:' | sed 's/.*: //')
CODE=$(curl -s -o /dev/null -w "%{http_code}" "$MEDIA_URL")
check "media GET" "200" "$CODE"

CODE=$(curl -s -o /dev/null -w "%{http_code}" -H "Range: bytes=0-100" "$MEDIA_URL")
check "media Range" "206" "$CODE"

CODE=$(curl -s -o /dev/null -w "%{http_code}" -H "Authorization: Bearer $API_KEY" -X DELETE "$BASE/api/desktop/video/delete?videoId=$VIDEO_ID")
check "video delete" "200" "$CODE"

echo ""
echo "== changelog =="

RESP=$(curl -s "$BASE/api/changelog/status?version=1.0.0")
check "changelog status" "false" "$(echo "$RESP" | json_get hasUpdate)"

RESP=$(curl -s "$BASE/api/changelog?origin=cap")
check "changelog posts" "[]" "$RESP"

echo ""
echo "PASS=$PASS FAIL=$FAIL"
[[ $FAIL -eq 0 ]]