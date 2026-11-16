-- Remove the module from session_preload_libraries, or reset the
-- setting where nothing else is left in it.
--start_ignore
\! v="$(psql -At -c "SELECT array_to_string(array_remove(string_to_array(current_setting('session_preload_libraries'), ','), 'pg_pmu'), ',')" postgres)"; if [ -z "$v" ]; then gpconfig -r session_preload_libraries; else gpconfig -c session_preload_libraries -v "$v"; fi
\! gpstop -u
\! sleep 1
--end_ignore
