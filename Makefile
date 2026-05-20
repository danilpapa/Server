db:
	@chmod +x ./scripts/setup.sh
	@./scripts/setup.sh &

server:
	@cargo run

wait-db:
	@echo "Waiting for DB..."
	@until nc -z localhost 5432; do sleep 1; done

all: db wait-db server