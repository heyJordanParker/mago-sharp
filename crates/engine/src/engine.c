#include <stddef.h>

#include <sapi/embed/php_embed.h>
#include <main/php_main.h>
#include <main/php_ini.h>
#include <main/php_variables.h>
#include <Zend/zend_API.h>
#include <Zend/zend_exceptions.h>
#include <Zend/zend_interfaces.h>
#include <ext/spl/spl_exceptions.h>

_Static_assert(sizeof(zval) == 16, "mago-engine's Zval holds a zval in 16 bytes");

enum engine_status {
	ENGINE_DONE = 0,
	ENGINE_THREW = 1,
	ENGINE_BAILED_OUT = 2,
};

extern int mago_engine_answer(zval *arguments, uint32_t count, zval *result);
extern void mago_engine_output(const char *bytes, size_t length);
extern const char *mago_engine_variable(const char *name, size_t length);
extern bool mago_engine_variable_at(size_t index, const char **name, const char **value);

ZEND_BEGIN_ARG_WITH_RETURN_TYPE_INFO_EX(arginfo_ask, 0, 0, IS_MIXED, 0)
	ZEND_ARG_VARIADIC_TYPE_INFO(0, arguments, IS_MIXED, 0)
ZEND_END_ARG_INFO()

static ZEND_FUNCTION(ask)
{
	zval *arguments = NULL;
	uint32_t count = 0;
	ZEND_PARSE_PARAMETERS_START(0, -1)
		Z_PARAM_VARIADIC('*', arguments, count)
	ZEND_PARSE_PARAMETERS_END();

	if (mago_engine_answer(arguments, count, return_value) != 0) {
		zend_throw_exception(spl_ce_RuntimeException, Z_STRVAL_P(return_value), 0);
		zval_ptr_dtor(return_value);
		ZVAL_NULL(return_value);
	}
}

ZEND_BEGIN_ARG_WITH_RETURN_TYPE_INFO_EX(arginfo_classCount, 0, 0, IS_LONG, 0)
ZEND_END_ARG_INFO()

/* How many classes, interfaces, traits and enums the interpreter has declared, which grows with each one it declares. */
static ZEND_FUNCTION(classCount)
{
	ZEND_PARSE_PARAMETERS_NONE();

	RETURN_LONG(zend_hash_num_elements(EG(class_table)));
}

static const zend_function_entry engine_functions[] = {
	ZEND_NS_FE("Mago\\Engine", ask, arginfo_ask)
	ZEND_NS_FE("Mago\\Engine", classCount, arginfo_classCount)
	ZEND_FE_END
};

static zend_module_entry engine_module = {
	STANDARD_MODULE_HEADER,
	"mago-engine",
	engine_functions,
	NULL,
	NULL,
	NULL,
	NULL,
	NULL,
	"1",
	STANDARD_MODULE_PROPERTIES
};

static int engine_module_startup(sapi_module_struct *sapi)
{
	return php_module_startup(sapi, &engine_module);
}

static size_t engine_write(const char *bytes, size_t length)
{
	mago_engine_output(bytes, length);
	return length;
}

static void engine_log(const char *message, int syslog_type)
{
	(void) syslog_type;
	mago_engine_output(message, strlen(message));
	mago_engine_output("\n", 1);
}

static void engine_ini_default(HashTable *configuration, const char *name, size_t length, const char *value)
{
	zval setting;
	ZVAL_NEW_STR(&setting, zend_string_init(value, strlen(value), 1));
	zend_hash_str_update(configuration, name, length, &setting);
}

static void engine_ini_defaults(HashTable *configuration)
{
	engine_ini_default(configuration, ZEND_STRL("hard_timeout"), "0");
	engine_ini_default(configuration, ZEND_STRL("memory_limit"), "-1");
}

/* getenv() reads the engine's variables before the process's own. */
static char *engine_getenv(const char *name, size_t length)
{
	return (char *) mago_engine_variable(name, length);
}

static void (*engine_process_variables)(zval *array);

/* $_ENV and $_SERVER hold the process's environment, then the engine's variables over it. */
static void engine_import_variables(zval *array)
{
	engine_process_variables(array);
	const char *name;
	const char *value;
	for (size_t index = 0; mago_engine_variable_at(index, &name, &value); index++) {
		zval entry;
		ZVAL_STRING(&entry, value);
		zend_hash_str_update(Z_ARRVAL_P(array), name, strlen(name), &entry);
	}
}

static zend_internal_function engine_caller;

int engine_start(void)
{
	php_embed_module.startup = engine_module_startup;
	php_embed_module.ub_write = engine_write;
	php_embed_module.log_message = engine_log;
	php_embed_module.ini_defaults = engine_ini_defaults;
	php_embed_module.php_ini_ignore = 1;
	php_embed_module.getenv = engine_getenv;
	engine_process_variables = php_import_environment_variables;
	php_import_environment_variables = engine_import_variables;
	if (php_embed_init(0, NULL) == FAILURE) {
		return -1;
	}

	engine_caller.type = ZEND_INTERNAL_FUNCTION;
	engine_caller.function_name = zend_string_init_interned(ZEND_STRL("Mago\\Engine\\call"), 1);
	php_request_shutdown(NULL);
	return 0;
}

int engine_interpreter_start(void)
{
	ts_resource(0);
	ZEND_TSRMLS_CACHE_UPDATE();
	if (php_request_startup() == FAILURE) {
		return -1;
	}

	SG(headers_sent) = 1;
	SG(request_info).no_headers = 1;
	return 0;
}

/* Sets the PHP setting name to value for the running interpreter's request, as `php -d` sets it, a setting only
 * php.ini may change included. */
int engine_set(const char *name, size_t name_length, const char *value, size_t value_length)
{
	zend_string *setting = zend_string_init(name, name_length, 0);
	zend_result set = zend_alter_ini_entry_chars(setting, value, value_length, ZEND_INI_ALL, ZEND_INI_STAGE_ACTIVATE);
	zend_string_release(setting);

	return set == SUCCESS ? 0 : -1;
}

void engine_interpreter_stop(void)
{
	php_request_shutdown(NULL);
}

void engine_thread_end(void)
{
	if (!tsrm_is_main_thread()) {
		ts_free_thread();
	}
}

static void engine_describe_exception(zval *result)
{
	zend_object *exception = EG(exception);
	zend_string *message = zval_get_string(
		zend_read_property_ex(exception->ce, exception, ZSTR_KNOWN(ZEND_STR_MESSAGE), 1, &EG(uninitialized_zval)));
	zend_string *file = zval_get_string(
		zend_read_property_ex(exception->ce, exception, ZSTR_KNOWN(ZEND_STR_FILE), 1, &EG(uninitialized_zval)));
	zend_long line = zval_get_long(
		zend_read_property_ex(exception->ce, exception, ZSTR_KNOWN(ZEND_STR_LINE), 1, &EG(uninitialized_zval)));
	ZVAL_STR(result, zend_strpprintf(
		0, "%s: %s in %s:" ZEND_LONG_FMT, ZSTR_VAL(exception->ce->name), ZSTR_VAL(message), ZSTR_VAL(file), line));
	zend_string_release(message);
	zend_string_release(file);
	zend_clear_exception();
}

int engine_run_script(const char *path, zval *result)
{
	/* zend_catch returns here through longjmp, so status lives in memory, where the assignments before it stay. */
	volatile int status = ENGINE_DONE;
	ZVAL_NULL(result);
	zend_try {
		zend_file_handle handle;
		zend_stream_init_filename(&handle, path);
		zend_op_array *script = zend_compile_file(&handle, ZEND_REQUIRE);
		zend_destroy_file_handle(&handle);
		if (script == NULL) {
			status = EG(exception) ? ENGINE_THREW : ENGINE_BAILED_OUT;
		} else {
			zval returned;
			ZVAL_UNDEF(&returned);
			zend_execute(script, &returned);
			zend_destroy_static_vars(script);
			destroy_op_array(script);
			efree_size(script, sizeof(zend_op_array));
			zval_ptr_dtor(&returned);
			if (EG(exception)) {
				status = ENGINE_THREW;
			}
		}
		if (status == ENGINE_THREW) {
			engine_describe_exception(result);
		}
	} zend_catch {
		status = ENGINE_BAILED_OUT;
	} zend_end_try();
	return status;
}

int engine_call(const char *function, size_t length, zval *arguments, uint32_t count, zval *result)
{
	/* zend_catch returns here through longjmp, so status lives in memory, where the assignments before it stay. */
	volatile int status = ENGINE_DONE;
	ZVAL_NULL(result);
	zend_execute_data caller = {0};
	caller.func = (zend_function *) &engine_caller;
	EG(current_execute_data) = &caller;
	zend_try {
		zval name;
		ZVAL_STRINGL(&name, function, length);
		if (call_user_function(NULL, NULL, &name, result, count, arguments) == FAILURE) {
			status = ENGINE_BAILED_OUT;
		} else if (EG(exception)) {
			zval_ptr_dtor(result);
			engine_describe_exception(result);
			status = ENGINE_THREW;
		}
		zval_ptr_dtor(&name);
	} zend_catch {
		status = ENGINE_BAILED_OUT;
	} zend_end_try();
	EG(current_execute_data) = NULL;
	return status;
}

zend_executor_globals *engine_executor_globals(void)
{
	return (zend_executor_globals *) ((char *) &EG(vm_interrupt) - offsetof(zend_executor_globals, vm_interrupt));
}

void engine_interrupt(zend_executor_globals *globals)
{
	zend_atomic_bool_store(&globals->timed_out, true);
	zend_atomic_bool_store(&globals->vm_interrupt, true);
}

void engine_clear_interrupt(zend_executor_globals *globals)
{
	zend_atomic_bool_store(&globals->timed_out, false);
	zend_atomic_bool_store(&globals->vm_interrupt, false);
}

void engine_value_null(zval *value)
{
	ZVAL_NULL(value);
}

void engine_value_bool(zval *value, bool flag)
{
	ZVAL_BOOL(value, flag);
}

void engine_value_int(zval *value, int64_t number)
{
	ZVAL_LONG(value, number);
}

void engine_value_float(zval *value, double number)
{
	ZVAL_DOUBLE(value, number);
}

void engine_value_string(zval *value, const char *bytes, size_t length)
{
	ZVAL_STRINGL_FAST(value, bytes, length);
}

void engine_value_array(zval *value, uint32_t size)
{
	array_init_size(value, size);
}

zval *engine_array_push(zval *array)
{
	zval slot;
	ZVAL_NULL(&slot);
	return zend_hash_next_index_insert(Z_ARRVAL_P(array), &slot);
}

zval *engine_array_insert(zval *array, const char *key, size_t length)
{
	zval slot;
	ZVAL_NULL(&slot);
	return zend_symtable_str_update(Z_ARRVAL_P(array), key, length, &slot);
}

void engine_value_release(zval *value)
{
	zval_ptr_dtor(value);
}

uint8_t engine_value_type(const zval *value)
{
	return Z_TYPE_P(value) == IS_REFERENCE ? Z_TYPE_P(Z_REFVAL_P(value)) : Z_TYPE_P(value);
}

static const zval *engine_dereference(const zval *value)
{
	return Z_TYPE_P(value) == IS_REFERENCE ? Z_REFVAL_P(value) : value;
}

int64_t engine_value_long(const zval *value)
{
	return Z_LVAL_P(engine_dereference(value));
}

double engine_value_double(const zval *value)
{
	return Z_DVAL_P(engine_dereference(value));
}

const char *engine_value_bytes(const zval *value, size_t *length)
{
	const zval *string = engine_dereference(value);
	*length = Z_STRLEN_P(string);
	return Z_STRVAL_P(string);
}

bool engine_array_is_list(const zval *value)
{
	return zend_array_is_list(Z_ARRVAL_P(engine_dereference(value)));
}

uint32_t engine_array_count(const zval *value)
{
	return zend_hash_num_elements(Z_ARRVAL_P(engine_dereference(value)));
}

bool engine_array_entry(
	const zval *array, uint32_t *position, const zval **value, const char **key, size_t *key_length, int64_t *index)
{
	HashTable *table = Z_ARRVAL_P(engine_dereference(array));
	HashPosition cursor = *position;
	zval *entry = zend_hash_get_current_data_ex(table, &cursor);
	if (entry == NULL) {
		return false;
	}

	zend_string *string_key = NULL;
	zend_ulong number_key = 0;
	zend_hash_get_current_key_ex(table, &string_key, &number_key, &cursor);
	*value = entry;
	*key = string_key == NULL ? NULL : ZSTR_VAL(string_key);
	*key_length = string_key == NULL ? 0 : ZSTR_LEN(string_key);
	*index = (int64_t) number_key;
	zend_hash_move_forward_ex(table, &cursor);
	*position = cursor;
	return true;
}
