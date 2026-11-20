// Starts PHP (the embed SAPI) <runs> times in this process, running <script> each time: as
// FrankenPHP restarts PHP, the extensions are started again while their modules stay loaded.
//
// Usage: restart <script> <runs>

// PHP's headers use memrchr().
#define _GNU_SOURCE
#include <stdlib.h>
#include <sapi/embed/php_embed.h>

int main(int argc, char **argv)
{
	if (argc != 3) {
		return 2;
	}
	int runs = atoi(argv[2]);
	for (int i = 0; i < runs; i++) {
		PHP_EMBED_START_BLOCK(argc, argv)
			zend_file_handle script;
			zend_stream_init_filename(&script, argv[1]);
			php_execute_script(&script);
			zend_destroy_file_handle(&script);
		PHP_EMBED_END_BLOCK()
	}
	return 0;
}
