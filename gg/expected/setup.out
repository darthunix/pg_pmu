-- Load the module in every new backend of the cluster, the QEs' too:
-- LOAD reaches only the backends running now.
--start_ignore
\! gpconfig -c session_preload_libraries -v "$(psql -At -c "SELECT array_to_string(array_append(string_to_array(current_setting('session_preload_libraries'), ','), 'pg_pmu'), ',')" postgres)"
\! gpstop -u
\! sleep 1
--end_ignore
