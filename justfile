
run:
	RUST_LOG=pcotool=debug cargo run
container:
	nix build "./#container"
	docker load -i result
	docker run --rm --init --env-file .env pcotool
