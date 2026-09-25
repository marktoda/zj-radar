//! The shared case corpus for the background-shell service classifier
//! (`agents::shell_is_service`, mirrored in notify.sh's jq): the Rust unit
//! test below and parity.bats (which runs every case through BOTH producers
//! as a `Stop` snapshot) read the same lines, so the two can't drift.
//!
//! One case per line: `service` or `bounded`, a TAB, the command, and
//! optionally a TAB and the description. An empty command field means no
//! command. In the command, `\t` and `\n` stand for a tab and a newline.
//! Blank lines and `#` comments are skipped. Keep `"#` out of the text (it
//! would end the raw string).

pub(crate) const SERVICE_CASES: &str = r#"
# ── Servers, watchers, tunnels, followed logs ──
service	python -m http.server 8000
service	/usr/bin/python3 -m http.server
service	make dev
service	just dev
service	make server
service	npx vite
service	vite
service	vite dev
service	npx vite dev
service	pnpm vite preview
service	./node_modules/.bin/vite serve
service	pnpm exec vite --host
service	bun x vite@latest
service	uvicorn app:app --reload
service	./venv/bin/uvicorn app:app
service	gunicorn app:wsgi
service	flask run
service	rails s
service	bin/rails s
service	bin/rails server
service	bundle exec rails s
service	python manage.py runserver
service	./manage.py runserver
service	cargo watch -x test
service	watchexec -e rs cargo test
service	tsc --watch
service	npx tsc --watch
service	jest --watch
service	jest --watchAll
service	vitest --watch=true
service	kubectl port-forward svc/db 5432
service	kubectl get pods --watch
service	nodemon index.js
service	npx nodemon@3 index.js
service	bundle exec jekyll serve
service	mkdocs serve
service	hugo server -D
service	npx serve -s build
service	pnpm serve
service	npm run serve
service	tail -f log
service	tail -F x.log
service	docker compose up
service	docker-compose up web
service	npm start
service	npm run dev
service	npm run start
service	yarn dev
service	pnpm dev
service	bun run dev
service	npx next dev
service	npm --prefix web run dev
# ── Segments: & | ; newline CR ( ) ` each start a command ──
service	cd web && vite
service	cd web&&vite
service	cd web && PNPM run dev
service	npm run build; vite
service	cd web\nvite
service	npm run dev|tee log
service	npm run dev > dev.log 2>&1
service	(npm run dev)
service	echo `npm start`
service	echo $(npm start)
service	npm run dev\techo
service	gh pr checks 57 && tsc --watch
service	gh pr checks 57\ntsc --watch
service	tsc --watch; gh pr view
# ── Heads past env assignments and wrappers; bash -c's argument is read ──
service	FOO=1 npm run dev
service	env PORT=3000 npm start
service	nohup npm run dev
service	time vite
service	exec uvicorn app:app
service	sudo -E npm start
service	bash -c "npm run dev"
service	sh -c 'npm run dev'
service	bash -lc "cd web && vite"
# ── vite without build, --watch not switched off ──
bounded	npx vite build
bounded	vite build --mode prod
bounded	vite --mode staging build
bounded	vite build&&echo ok
bounded	npx -p vite vite build
bounded	jest --watch=false
bounded	vitest --watch=0
bounded	jest --watch false
bounded	vitest --watch 0
# ── Service words outside a command position ──
bounded	npm install vite
bounded	pnpm add -D vite
bounded	ls node_modules/vite
bounded	cd packages/vite && pnpm test
bounded	echo vite
bounded	git commit -m "run vite"
bounded	pytest tests/test_serve.py
bounded	pytest tests/test_uvicorn_app.py
bounded	go test ./serve/...
bounded	go test ./serve
bounded	cd ~/code/serve && cargo test
bounded	cd serve && cargo test
bounded	cd packages/uvicorn && pytest
bounded	cargo test -p serve
bounded	cargo test -p server
bounded	make dev-deps
bounded	just dev-setup
bounded	npm run dev:migrate
bounded	npm run watch-docs-check
bounded	ls serve.d
bounded	cat .serve
bounded	./httpxserver
bounded	./watch.sh
bounded	vitest run
bounded	CI=1 npx vitest run
bounded	gh run watch 123
# ── gh / kubectl rollout status watches end ──
bounded	gh pr checks 57 --watch
bounded	gh pr checks 57 --watch && gh pr merge
bounded	/usr/bin/gh run list --watch
bounded	kubectl rollout status deploy/x --watch
bounded	kubectl rollout status deploy/x --watch=true
# ── Detached or exit-coupled compose ends ──
bounded	docker compose up -d
bounded	docker compose up --detach
bounded	docker compose up --abort-on-container-exit
bounded	docker compose up --exit-code-from=tests
bounded	docker compose up --exit-code-from tests
# ── A command decides alone; the description only without one ──
bounded	npx playwright test	Run e2e tests against the dev server
bounded	CI=1 npx vitest run	Run vitest (non-watch mode)
bounded	./bin/app --port 3000	Start the dev server
bounded	cargo test -p server	Run the server tests
bounded	cargo test	Run tests and watch for failures
bounded	cargo nextest run	Run the test suite
service	npm run dev	Run the test suite
service		Start the dev server
service		Rebuild in watch mode
service		Run the docs dev server
bounded		Run the test suite
bounded		Serve up the test report
bounded		Run tests and watch for failures
# Neither: nothing says it is bounded.
service
# Scripts through a runner: `dev`/`start` are services, other scripts end.
service	pnpm --filter web dev
service	pnpm -r --parallel dev
service	uv run uvicorn app:app
service	poetry run uvicorn app:app
service	npm run start
bounded	npm run test
bounded	poetry run pytest
bounded	uv run pytest -q
bounded	yarn run build
bounded	npm run dev:migrate
# ── Known misses (a miss only holds "waiting on …" until the next Stop) ──
bounded	docker compose -f dev.yml up
bounded	yarn workspace web start
bounded	kubectl -n prod port-forward svc/db 5432
"#;
