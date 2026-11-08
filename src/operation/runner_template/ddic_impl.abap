  METHOD run_type.
    IF mv_type = `TRAN`.
      reject( `This runner has no template for the requested type` ).
      RETURN.
    ENDIF.
    mv_ddname = mv_name.
    CASE mv_mode.
      WHEN `READ`.
        do_read( ).
      WHEN `SIMULATE` OR `DEPLOY`.
        do_deploy( ).
      WHEN `CREATE_SIM` OR `CREATE`.
        do_create( ).
      WHEN `DELETE_SIM` OR `DELETE`.
        do_delete( ).
      WHEN OTHERS.
        reject( |A DDIC object has no { mv_mode } mode| ).
    ENDCASE.
  ENDMETHOD.


  METHOD do_read.
    " The active definition as the canonical XML rows, any name, any table class.
    IF read_live( ) = abap_false.
      msg( iv_level = `E` iv_text = |{ mv_type } { mv_name } active version not found| ).
      finish( `NOT_FOUND` ).
      RETURN.
    ENDIF.
    IF emit_rows( ) = abap_false.
      RETURN.
    ENDIF.
    fact( iv_key = `HASH` iv_text = mv_live_hash ).
    finish( `OK` ).
  ENDMETHOD.


  METHOD do_deploy.
    " SIMULATE reports and ends; DEPLOY takes the DDIC lock, checks the base under it, and writes.
    DATA:
      lt_file TYPE string_table.

    DATA:
      lv_live_hash TYPE string.

    IF read_live( ) = abap_false.
      reject( |{ mv_type } { mv_name } active version not found| ).
      RETURN.
    ENDIF.
    lv_live_hash = mv_live_hash.
    fact( iv_key = `HASH` iv_text = lv_live_hash ).
    IF mv_mode = `SIMULATE` AND emit_rows( ) = abap_false.
      RETURN.
    ENDIF.
    IF object_package( ) = abap_false.
      RETURN.
    ENDIF.
    fact( iv_key = `PACKAGE` iv_text = |{ mv_devclass }| ).
    IF mv_type = `TABL` AND check_tabclass( live_class( ) ) = abap_false.
      RETURN.
    ENDIF.
    IF check_inactive( ) = abap_false.
      RETURN.
    ENDIF.
    IF mv_mode = `DEPLOY` AND lock_object( ) = abap_false.
      RETURN.
    ENDIF.
    IF check_base( lv_live_hash ) = abap_false.
      RETURN.
    ENDIF.
    IF parse_file( ) = abap_false.
      RETURN.
    ENDIF.
    lt_file = mt_xml.
    normalize_rows( CHANGING ct_rows = lt_file ).
    IF rows_hash( lt_file ) = lv_live_hash.
      msg( iv_level = `I` iv_text = `No change: file equals active definition` ).
      IF mv_mode = `SIMULATE`.
        finish( `NO_CHANGES` ).
      ELSE.
        fact( iv_key = `RESULT_HASH` iv_text = lv_live_hash ).
        finish( `ACTIVE` ).
      ENDIF.
      RETURN.
    ENDIF.
    IF check_transport( |{ mv_devclass }| ) = abap_false.
      RETURN.
    ENDIF.
    IF check_changes( ) = abap_false.
      RETURN.
    ENDIF.
    IF mv_mode = `SIMULATE`.
      emit_where_used( ).
      emit_guards( ).
      finish( `SIMULATED_OK` ).
      RETURN.
    ENDIF.
    " The verdict rolled back its buffered writes; the base is read once more right before the write.
    IF read_live( ) = abap_false OR mv_live_hash <> lv_live_hash.
      reject( `Base changed since the diff was shown; run deploy again` ).
      RETURN.
    ENDIF.
    write_definition( abap_false ).
  ENDMETHOD.


  METHOD do_create.
    IF create_package( ) = abap_false.
      RETURN.
    ENDIF.
    IF exists_object( abap_true ) = abap_true.
      reject( |{ mv_type } { mv_name } already exists| ).
      RETURN.
    ENDIF.
    IF check_transport( |{ mv_devclass }| ) = abap_false.
      RETURN.
    ENDIF.
    IF parse_file( ) = abap_false.
      RETURN.
    ENDIF.
    " Transparent table names are limited to 16 characters by the database layer.
    IF mv_type = `TABL` AND ms_file-dd02v-tabclass = 'TRANSP' AND strlen( mv_name ) > 16.
      reject( |Transparent table name { mv_name } > 16 characters| ).
      RETURN.
    ENDIF.
    msg( iv_level = `I`
         iv_text = |CREATE { mv_type } { mv_name } package { mv_devclass } TR { COND string( WHEN mv_local = abap_true THEN `-` ELSE mv_tr ) }| ).
    IF mv_mode = `CREATE_SIM`.
      fact( iv_key = `PACKAGE` iv_text = |{ mv_devclass }| ).
      emit_guards( ).
      finish( `SIMULATED_OK` ).
      RETURN.
    ENDIF.
    write_definition( abap_true ).
  ENDMETHOD.


  METHOD do_delete.
    DATA:
      lt_none TYPE string_table.

    DATA:
      lv_object  TYPE trobjtype,
      lv_objname TYPE sobj_name,
      lv_author  TYPE tadir-author,
      lv_state   TYPE string,
      lv_hash    TYPE string.

    IF exists_object( abap_false ) = abap_false.
      msg( iv_level = `E` iv_text = |Object { mv_type } { mv_name } does not exist| ).
      finish( `NOT_FOUND` ).
      RETURN.
    ENDIF.
    lv_object = mv_type.
    lv_objname = mv_name.
    SELECT SINGLE devclass, author FROM tadir
      WHERE pgmid = 'R3TR' AND object = @lv_object AND obj_name = @lv_objname AND delflag = @space
      INTO ( @mv_devclass, @lv_author ).
    IF sy-subrc <> 0.
      msg( iv_level = `E` iv_text = |Object { mv_type } { mv_name } does not exist (TADIR R3TR { mv_type } { mv_name })| ).
      finish( `NOT_FOUND` ).
      RETURN.
    ENDIF.
    mv_local = xsdbool( substring( val = |{ mv_devclass }| len = 1 ) = `$` ).
    IF mv_type = `TABL` AND check_tabclass( live_class( ) ) = abap_false.
      RETURN.
    ENDIF.
    IF check_delete( ) = abap_false.
      RETURN.
    ENDIF.
    IF delete_request( ) = abap_false.
      RETURN.
    ENDIF.
    " The plan hash is the hash of the live XML rows; a definition without an active version
    " hashes the single row NO_ACTIVE.
    IF read_live( ) = abap_true.
      lv_state = `ACTIVE`.
      lv_hash = mv_live_hash.
    ELSE.
      lv_state = `NO_ACTIVE`.
      APPEND `NO_ACTIVE` TO lt_none.
      lv_hash = rows_hash( lt_none ).
    ENDIF.
    IF mv_mode = `DELETE_SIM`.
      fact( iv_key = `PACKAGE` iv_text = |{ mv_devclass }| ).
      fact( iv_key = `AUTHOR` iv_text = |{ lv_author }| ).
      fact( iv_key = `REQUEST` iv_text = COND #( WHEN mv_tr IS INITIAL THEN `-` ELSE mv_tr ) ).
      fact( iv_key = `STATE` iv_text = lv_state ).
      fact( iv_key = `HASH` iv_text = lv_hash ).
      emit_where_used( ).
      emit_guards( ).
      finish( `SIMULATED_OK` ).
      RETURN.
    ENDIF.
    IF mv_plan <> lv_hash.
      reject( `Plan changed since simulate; simulate again` ).
      RETURN.
    ENDIF.
    delete_definition( ).
  ENDMETHOD.


  METHOD get_def.
    " The active version in the logon language, as the Python tool reads it.
    DATA:
      lv_state TYPE ddgotstate,
      lv_name  TYPE ddobjname.

    CLEAR: es_def, ev_found.
    lv_name = iv_name.
    CASE iv_type.
      WHEN `DTEL`.
        CALL FUNCTION 'DDIF_DTEL_GET'
          EXPORTING
            name          = lv_name
            state         = 'A'
            langu         = sy-langu
          IMPORTING
            gotstate      = lv_state
            dd04v_wa      = es_def-dd04v
          EXCEPTIONS
            illegal_input = 1
            OTHERS        = 2.
      WHEN `DOMA`.
        CALL FUNCTION 'DDIF_DOMA_GET'
          EXPORTING
            name          = lv_name
            state         = 'A'
            langu         = sy-langu
          IMPORTING
            gotstate      = lv_state
            dd01v_wa      = es_def-dd01v
          TABLES
            dd07v_tab     = es_def-dd07v
          EXCEPTIONS
            illegal_input = 1
            OTHERS        = 2.
      WHEN `TABL`.
        CALL FUNCTION 'DDIF_TABL_GET'
          EXPORTING
            name          = lv_name
            state         = 'A'
            langu         = sy-langu
          IMPORTING
            gotstate      = lv_state
            dd02v_wa      = es_def-dd02v
            dd09l_wa      = es_def-dd09l
          TABLES
            dd03p_tab     = es_def-dd03p
            dd05m_tab     = es_def-dd05m
            dd08v_tab     = es_def-dd08v
            dd35v_tab     = es_def-dd35v
            dd36m_tab     = es_def-dd36m
          EXCEPTIONS
            illegal_input = 1
            OTHERS        = 2.
      WHEN `TTYP`.
        CALL FUNCTION 'DDIF_TTYP_GET'
          EXPORTING
            name          = lv_name
            state         = 'A'
            langu         = sy-langu
          IMPORTING
            gotstate      = lv_state
            dd40v_wa      = es_def-dd40v
          TABLES
            dd42v_tab     = es_def-dd42v
            dd43v_tab     = es_def-dd43v
          EXCEPTIONS
            illegal_input = 1
            OTHERS        = 2.
      WHEN OTHERS.
        RETURN.
    ENDCASE.
    IF sy-subrc = 0 AND lv_state = 'A'.
      ev_found = abap_true.
    ENDIF.
  ENDMETHOD.


  METHOD canonicalize.
    " Fields SAP derives or stamps are dropped, so a file read back stays stable and a changed
    " ROLLNAME does not keep the old type.
    CLEAR: cs_def-dd04v-as4user, cs_def-dd04v-as4date, cs_def-dd04v-as4time,
           cs_def-dd01v-as4user, cs_def-dd01v-as4date, cs_def-dd01v-as4time,
           cs_def-dd02v-as4user, cs_def-dd02v-as4date, cs_def-dd02v-as4time,
           cs_def-dd09l-as4user, cs_def-dd09l-as4date, cs_def-dd09l-as4time,
           cs_def-dd09l-as4local, cs_def-dd09l-as4vers,
           cs_def-dd40v-as4user, cs_def-dd40v-as4date, cs_def-dd40v-as4time.
    " A data element based on a domain takes its type from the domain.
    IF cs_def-dd04v-domname IS NOT INITIAL.
      CLEAR: cs_def-dd04v-datatype, cs_def-dd04v-leng, cs_def-dd04v-decimals, cs_def-dd04v-outputlen,
             cs_def-dd04v-lowercase, cs_def-dd04v-signflag, cs_def-dd04v-convexit, cs_def-dd04v-valexi,
             cs_def-dd04v-entitytab.
    ENDIF.
    CLEAR cs_def-dd40v-typelen.
    " Fields taken over from includes and appends are generated by SAP.
    DELETE cs_def-dd03p WHERE adminfield <> '0'.
    LOOP AT cs_def-dd03p ASSIGNING FIELD-SYMBOL(<ls_dd03p>).
      IF <ls_dd03p>-rollname IS NOT INITIAL AND <ls_dd03p>-comptype <> 'R'.
        CLEAR: <ls_dd03p>-datatype, <ls_dd03p>-leng, <ls_dd03p>-decimals,
               <ls_dd03p>-inttype, <ls_dd03p>-intlen, <ls_dd03p>-domname,
               <ls_dd03p>-outputlen, <ls_dd03p>-lowercase, <ls_dd03p>-signflag,
               <ls_dd03p>-convexit, <ls_dd03p>-entitytab, <ls_dd03p>-shlporigin,
               <ls_dd03p>-memoryid, <ls_dd03p>-ddtext, <ls_dd03p>-reptext,
               <ls_dd03p>-scrtext_s, <ls_dd03p>-scrtext_m, <ls_dd03p>-scrtext_l,
               <ls_dd03p>-ddlanguage, <ls_dd03p>-dtelmaster, <ls_dd03p>-logflag,
               <ls_dd03p>-headlen, <ls_dd03p>-scrlen1, <ls_dd03p>-scrlen2,
               <ls_dd03p>-scrlen3, <ls_dd03p>-valexi, <ls_dd03p>-mask.
      ENDIF.
      CLEAR <ls_dd03p>-domname3l.
    ENDLOOP.
  ENDMETHOD.


  METHOD serialize.
    " The abapGit-like envelope around the asXML document, one element per row, one blank of
    " indent per level plus one for the envelope. The element names are the Python spelling.
    DATA:
      lx_error TYPE REF TO cx_root.

    DATA:
      lv_xml TYPE string.

    TRY.
        CASE mv_type.
          WHEN `DTEL`.
            CALL TRANSFORMATION id OPTIONS initial_components = 'suppress' xml_header = 'no'
              SOURCE dd04v = is_def-dd04v
              RESULT XML lv_xml.
          WHEN `DOMA`.
            CALL TRANSFORMATION id OPTIONS initial_components = 'suppress' xml_header = 'no'
              SOURCE dd01v = is_def-dd01v dd07v_tab = is_def-dd07v
              RESULT XML lv_xml.
          WHEN `TABL`.
            CALL TRANSFORMATION id OPTIONS initial_components = 'suppress' xml_header = 'no'
              SOURCE dd02v = is_def-dd02v dd09l = is_def-dd09l dd03p_table = is_def-dd03p
                     dd05m_table = is_def-dd05m dd08v_table = is_def-dd08v
                     dd35v_tale = is_def-dd35v dd36m = is_def-dd36m
              RESULT XML lv_xml.
          WHEN `TTYP`.
            CALL TRANSFORMATION id OPTIONS initial_components = 'suppress' xml_header = 'no'
              SOURCE dd40v = is_def-dd40v dd42v = is_def-dd42v dd43v = is_def-dd43v
              RESULT XML lv_xml.
        ENDCASE.
      CATCH cx_root INTO lx_error.
        msg( iv_level = `E` iv_text = |XML { mv_type } { mv_name } failed: { lx_error->get_text( ) }| ).
        RETURN.
    ENDTRY.
    APPEND `<?xml version="1.0" encoding="utf-8"?>` TO rt_rows.
    APPEND |<abapGit version="v1.0.0" serializer="LCL_OBJECT_{ mv_type }" serializer_version="v1.0.0">| TO rt_rows.
    APPEND LINES OF pretty( lv_xml ) TO rt_rows.
    APPEND `</abapGit>` TO rt_rows.
  ENDMETHOD.


  METHOD pretty.
    " One element per row. An element that holds only text stays on one row.
    DATA:
      lt_tags TYPE match_result_tab.

    DATA:
      lv_count TYPE i,
      lv_index TYPE i,
      lv_depth TYPE i,
      lv_start TYPE i,
      lv_gap   TYPE i,
      lv_tag   TYPE string,
      lv_close TYPE string,
      lv_next  TYPE string,
      lv_text  TYPE string,
      lv_pad   TYPE string.

    FIND ALL OCCURRENCES OF REGEX `<[^>]*>` IN iv_xml RESULTS lt_tags.
    lv_count = lines( lt_tags ).
    lv_index = 1.
    WHILE lv_index <= lv_count.
      lv_tag = substring( val = iv_xml off = lt_tags[ lv_index ]-offset len = lt_tags[ lv_index ]-length ).
      IF substring( val = lv_tag len = 2 ) = `</`.
        lv_depth = nmax( val1 = lv_depth - 1 val2 = 0 ).
        lv_pad = repeat( val = ` ` occ = lv_depth + 1 ).
        APPEND |{ lv_pad }{ lv_tag }| TO rt_lines.
      ELSEIF substring( val = lv_tag off = strlen( lv_tag ) - 2 len = 2 ) = `/>`.
        lv_pad = repeat( val = ` ` occ = lv_depth + 1 ).
        APPEND |{ lv_pad }{ lv_tag }| TO rt_lines.
      ELSE.
        lv_pad = repeat( val = ` ` occ = lv_depth + 1 ).
        " The next tag is read only after the test that it exists: a table expression behind AND
        " raises on the last tag instead of ending the condition.
        CLEAR lv_next.
        IF lv_index < lv_count.
          lv_next = substring( val = iv_xml off = lt_tags[ lv_index + 1 ]-offset len = 2 ).
        ENDIF.
        IF lv_next = `</`.
          lv_start = lt_tags[ lv_index ]-offset + lt_tags[ lv_index ]-length.
          lv_gap = lt_tags[ lv_index + 1 ]-offset - lv_start.
          lv_text = substring( val = iv_xml off = lv_start len = lv_gap ).
          lv_close = substring( val = iv_xml off = lt_tags[ lv_index + 1 ]-offset len = lt_tags[ lv_index + 1 ]-length ).
          APPEND |{ lv_pad }{ lv_tag }{ lv_text }{ lv_close }| TO rt_lines.
          lv_index = lv_index + 1.
        ELSE.
          APPEND |{ lv_pad }{ lv_tag }| TO rt_lines.
          lv_depth = lv_depth + 1.
        ENDIF.
      ENDIF.
      lv_index = lv_index + 1.
    ENDWHILE.
  ENDMETHOD.


  METHOD rows_hash.
    " Lower-case SHA-256 of the rows joined by line feeds, as UTF-8: the hash of the Rust side.
    DATA:
      lo_conv TYPE REF TO cl_abap_conv_out_ce.

    DATA:
      lv_x    TYPE xstring,
      lv_text TYPE string,
      lv_hash TYPE string,
      lv_lf   TYPE string.

    lv_lf = |\n|.
    lv_text = concat_lines_of( table = it_rows sep = lv_lf ).
    lo_conv = cl_abap_conv_out_ce=>create( encoding = 'UTF-8' ).
    lo_conv->convert( EXPORTING data = lv_text IMPORTING buffer = lv_x ).
    cl_abap_message_digest=>calculate_hash_for_raw(
      EXPORTING
        if_algorithm  = 'SHA256'
        if_data       = lv_x
      IMPORTING
        ef_hashstring = lv_hash ).
    rv_hash = to_lower( lv_hash ).
  ENDMETHOD.


  METHOD normalize_rows.
    " The rule of the client: no trailing blanks on a row, no empty rows at the end.
    DATA:
      lv_index TYPE i.

    LOOP AT ct_rows ASSIGNING FIELD-SYMBOL(<lv_row>).
      <lv_row> = replace( val = <lv_row> regex = ` +$` with = `` ).
    ENDLOOP.
    lv_index = lines( ct_rows ).
    WHILE lv_index > 0.
      IF ct_rows[ lv_index ] IS NOT INITIAL.
        EXIT.
      ENDIF.
      DELETE ct_rows INDEX lv_index.
      lv_index = lv_index - 1.
    ENDWHILE.
  ENDMETHOD.


  METHOD read_live.
    " The active definition as the canonical XML rows with their hash, in ms_live and mt_rows.
    DATA:
      lv_found TYPE abap_bool.

    get_def( EXPORTING iv_type = mv_type iv_name = mv_name IMPORTING es_def = ms_live ev_found = lv_found ).
    IF lv_found = abap_false.
      RETURN.
    ENDIF.
    canonicalize( CHANGING cs_def = ms_live ).
    mt_rows = serialize( ms_live ).
    IF mt_rows IS INITIAL.
      RETURN.
    ENDIF.
    normalize_rows( CHANGING ct_rows = mt_rows ).
    mv_live_hash = rows_hash( mt_rows ).
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD emit_rows.
    DATA:
      lv_row TYPE string.

    LOOP AT mt_rows INTO lv_row.
      IF strlen( lv_row ) > 255.
        reject( |XML row longer than 255 characters (row { sy-tabix })| ).
        RETURN.
      ENDIF.
    ENDLOOP.
    LOOP AT mt_rows INTO lv_row.
      data_line( lv_row ).
    ENDLOOP.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD parse_file.
    " Only the asx:abap subtree is a valid input for the ID transformation. The file must
    " describe exactly this object.
    DATA:
      lx_error TYPE REF TO cx_root.

    DATA:
      lv_text      TYPE string,
      lv_xml       TYPE string,
      lv_lf        TYPE string,
      lv_file_name TYPE string,
      lv_ddtext    TYPE string,
      lv_from      TYPE i,
      lv_to        TYPE i.

    lv_lf = |\n|.
    lv_text = concat_lines_of( table = mt_xml sep = lv_lf ).
    FIND FIRST OCCURRENCE OF `<asx:abap` IN lv_text MATCH OFFSET lv_from.
    IF sy-subrc <> 0.
      reject( `XML file without <asx:abap>` ).
      RETURN.
    ENDIF.
    FIND FIRST OCCURRENCE OF `</asx:abap>` IN lv_text MATCH OFFSET lv_to.
    IF sy-subrc <> 0 OR lv_to < lv_from.
      reject( `XML file without <asx:abap>` ).
      RETURN.
    ENDIF.
    lv_xml = substring( val = lv_text off = lv_from len = lv_to - lv_from + 11 ).
    CLEAR ms_file.
    TRY.
        CASE mv_type.
          WHEN `DTEL`.
            CALL TRANSFORMATION id SOURCE XML lv_xml
              RESULT dd04v = ms_file-dd04v.
          WHEN `DOMA`.
            CALL TRANSFORMATION id SOURCE XML lv_xml
              RESULT dd01v = ms_file-dd01v dd07v_tab = ms_file-dd07v.
          WHEN `TABL`.
            CALL TRANSFORMATION id SOURCE XML lv_xml
              RESULT dd02v = ms_file-dd02v dd09l = ms_file-dd09l dd03p_table = ms_file-dd03p
                     dd05m_table = ms_file-dd05m dd08v_table = ms_file-dd08v
                     dd35v_tale = ms_file-dd35v dd36m = ms_file-dd36m.
          WHEN `TTYP`.
            CALL TRANSFORMATION id SOURCE XML lv_xml
              RESULT dd40v = ms_file-dd40v dd42v = ms_file-dd42v dd43v = ms_file-dd43v.
        ENDCASE.
      CATCH cx_root INTO lx_error.
        reject( |XML file cannot be read: { lx_error->get_text( ) }| ).
        RETURN.
    ENDTRY.
    lv_file_name = SWITCH string( mv_type
                     WHEN `DTEL` THEN |{ ms_file-dd04v-rollname }|
                     WHEN `DOMA` THEN |{ ms_file-dd01v-domname }|
                     WHEN `TABL` THEN |{ ms_file-dd02v-tabname }|
                     WHEN `TTYP` THEN |{ ms_file-dd40v-typename }| ).
    IF lv_file_name <> mv_name.
      reject( |XML file is for { lv_file_name }, not { mv_type } { mv_name }| ).
      RETURN.
    ENDIF.
    IF mv_type = `TABL`.
      LOOP AT ms_file-dd03p INTO DATA(ls_row) WHERE tabname <> mv_ddname.
        reject( |XML file is for { ls_row-tabname }, not { mv_type } { mv_name }| ).
        RETURN.
      ENDLOOP.
      IF check_tabclass( ms_file-dd02v-tabclass ) = abap_false.
        RETURN.
      ENDIF.
    ENDIF.
    IF check_dtel_texts( ) = abap_false.
      RETURN.
    ENDIF.
    lv_ddtext = SWITCH string( mv_type
                  WHEN `DTEL` THEN |{ ms_file-dd04v-ddtext }|
                  WHEN `DOMA` THEN |{ ms_file-dd01v-ddtext }|
                  WHEN `TABL` THEN |{ ms_file-dd02v-ddtext }|
                  WHEN `TTYP` THEN |{ ms_file-dd40v-ddtext }| ).
    IF mv_text IS NOT INITIAL AND mv_text <> lv_ddtext.
      reject( `--text differs from the description in the XML file` ).
      RETURN.
    ENDIF.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD check_dtel_texts.
    " A text longer than its declared length makes the activation fail with rc 8, so the
    " lengths are checked before the PUT.
    IF mv_type <> `DTEL`.
      rv_ok = abap_true.
      RETURN.
    ENDIF.
    IF strlen( ms_file-dd04v-scrtext_s ) > ms_file-dd04v-scrlen1.
      reject( |Data element text SCRTEXT_S has { strlen( ms_file-dd04v-scrtext_s ) } characters but SCRLEN1 is { ms_file-dd04v-scrlen1 }| ).
      RETURN.
    ENDIF.
    IF strlen( ms_file-dd04v-scrtext_m ) > ms_file-dd04v-scrlen2.
      reject( |Data element text SCRTEXT_M has { strlen( ms_file-dd04v-scrtext_m ) } characters but SCRLEN2 is { ms_file-dd04v-scrlen2 }| ).
      RETURN.
    ENDIF.
    IF strlen( ms_file-dd04v-scrtext_l ) > ms_file-dd04v-scrlen3.
      reject( |Data element text SCRTEXT_L has { strlen( ms_file-dd04v-scrtext_l ) } characters but SCRLEN3 is { ms_file-dd04v-scrlen3 }| ).
      RETURN.
    ENDIF.
    IF strlen( ms_file-dd04v-reptext ) > ms_file-dd04v-headlen.
      reject( |Data element text REPTEXT has { strlen( ms_file-dd04v-reptext ) } characters but HEADLEN is { ms_file-dd04v-headlen }| ).
      RETURN.
    ENDIF.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD check_tabclass.
    " Only transparent tables and structures are written or deleted.
    IF iv_class <> 'TRANSP' AND iv_class <> 'INTTAB'.
      reject( |TABCLASS { iv_class } not supported| ).
      RETURN.
    ENDIF.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD live_class.
    SELECT SINGLE tabclass FROM dd02l
      WHERE tabname = @mv_ddname AND as4local = 'A'
      INTO @rv_class.
  ENDMETHOD.


  METHOD exists_object.
    " With iv_shared a data element, a table, and a table type share one name space.
    DATA:
      lv_count TYPE i.

    CASE mv_type.
      WHEN `DOMA`.
        SELECT COUNT(*) FROM dd01l WHERE domname = @mv_ddname INTO @lv_count.
      WHEN `DTEL`.
        SELECT COUNT(*) FROM dd04l WHERE rollname = @mv_ddname INTO @lv_count.
      WHEN `TABL`.
        SELECT COUNT(*) FROM dd02l WHERE tabname = @mv_ddname INTO @lv_count.
      WHEN `TTYP`.
        SELECT COUNT(*) FROM dd40l WHERE typename = @mv_ddname INTO @lv_count.
    ENDCASE.
    IF iv_shared = abap_true AND mv_type <> `DOMA`.
      IF lv_count = 0.
        SELECT COUNT(*) FROM dd04l WHERE rollname = @mv_ddname INTO @lv_count.
      ENDIF.
      IF lv_count = 0.
        SELECT COUNT(*) FROM dd02l WHERE tabname = @mv_ddname INTO @lv_count.
      ENDIF.
      IF lv_count = 0.
        SELECT COUNT(*) FROM dd40l WHERE typename = @mv_ddname INTO @lv_count.
      ENDIF.
    ENDIF.
    rv_exists = xsdbool( lv_count > 0 ).
  ENDMETHOD.


  METHOD object_package.
    " The package of an existing object is the one its TADIR entry names.
    DATA:
      lv_object  TYPE trobjtype,
      lv_objname TYPE sobj_name.

    lv_object = mv_type.
    lv_objname = mv_name.
    SELECT SINGLE devclass FROM tadir
      WHERE pgmid = 'R3TR' AND object = @lv_object AND obj_name = @lv_objname
      INTO @mv_devclass.
    IF sy-subrc <> 0.
      reject( |{ mv_type } { mv_name } has no TADIR R3TR { mv_type }| ).
      RETURN.
    ENDIF.
    mv_local = xsdbool( substring( val = |{ mv_devclass }| len = 1 ) = `$` ).
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD create_package.
    " The target package of a create; $TMP when none is given.
    DATA:
      lv_package TYPE devclass,
      lv_first   TYPE string,
      lv_count   TYPE i.

    lv_package = COND string( WHEN mv_package IS INITIAL THEN `$TMP` ELSE mv_package ).
    SELECT COUNT(*) FROM tdevc WHERE devclass = @lv_package INTO @lv_count.
    IF lv_count = 0.
      reject( |Package { lv_package } does not exist (TDEVC)| ).
      RETURN.
    ENDIF.
    lv_first = substring( val = |{ lv_package }| len = 1 ).
    IF lv_first <> `$` AND lv_first <> `Z` AND lv_first <> `Y`.
      reject( |Package { lv_package } is not $, Z, or Y| ).
      RETURN.
    ENDIF.
    mv_devclass = lv_package.
    mv_local = xsdbool( lv_first = `$` ).
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD count_inactive.
    " Inactive versions of anyone: DD rows of another state, or a working area entry.
    DATA:
      lv_name    TYPE ddobjname,
      lv_objname TYPE trobj_name,
      lv_objtype TYPE trobjtype.

    lv_name = iv_name.
    CASE iv_type.
      WHEN `DTEL`.
        SELECT COUNT(*) FROM dd04l WHERE rollname = @lv_name AND as4local <> 'A' INTO @rv_count.
      WHEN `DOMA`.
        SELECT COUNT(*) FROM dd01l WHERE domname = @lv_name AND as4local <> 'A' INTO @rv_count.
      WHEN `TABL`.
        SELECT COUNT(*) FROM dd02l WHERE tabname = @lv_name AND as4local <> 'A' INTO @rv_count.
        IF rv_count = 0.
          SELECT COUNT(*) FROM dd09l WHERE tabname = @lv_name AND as4local <> 'A' INTO @rv_count.
        ENDIF.
      WHEN `TTYP`.
        SELECT COUNT(*) FROM dd40l WHERE typename = @lv_name AND as4local <> 'A' INTO @rv_count.
    ENDCASE.
    IF rv_count = 0.
      lv_objname = iv_name.
      lv_objtype = iv_type.
      SELECT COUNT(*) FROM dwinactiv WHERE object = @lv_objtype AND obj_name = @lv_objname INTO @rv_count.
    ENDIF.
  ENDMETHOD.


  METHOD count_rows.
    " All clients: a conversion or a delete loses the rows of every client. A table without a
    " client field takes the plain count. A count that cannot be made is minus one, and every
    " caller refuses then.
    TRY.
        SELECT COUNT(*) FROM (iv_tabname) CLIENT SPECIFIED INTO @rv_rows.
      CATCH cx_root.
        TRY.
            SELECT COUNT(*) FROM (iv_tabname) INTO @rv_rows.
          CATCH cx_root.
            rv_rows = -1.
        ENDTRY.
    ENDTRY.
  ENDMETHOD.


  METHOD is_transparent.
    " The file or the active version: a file cannot switch the conversion guard off by
    " changing its TABCLASS.
    rv_transp = xsdbool( ms_file-dd02v-tabclass = 'TRANSP' OR live_class( ) = 'TRANSP' ).
  ENDMETHOD.


  METHOD tabclass_changed.
    DATA:
      lv_class TYPE dd02l-tabclass.

    SELECT SINGLE tabclass FROM dd02l
      WHERE tabname = @mv_ddname AND as4local = 'A'
      INTO @lv_class.
    rv_changed = xsdbool( sy-subrc = 0 AND lv_class <> ms_file-dd02v-tabclass
                          AND ( lv_class = 'TRANSP' OR ms_file-dd02v-tabclass = 'TRANSP' ) ).
  ENDMETHOD.


  METHOD check_changes.
    " The conversion rule of table activation for the table itself and, transitively, for the
    " transparent tables reached through a data element, a domain, or a structure. The verdict
    " may leave buffered writes; they are rolled back at the end.
    DATA:
      lv_action   TYPE tbatg-fct,
      lv_modeflag TYPE ddxtt-modeflag,
      lv_allowed  TYPE abap_bool,
      lv_decided  TYPE abap_bool,
      lv_rows     TYPE i.

    IF mv_type = `TABL`.
      IF tabclass_changed( ) = abap_true.
        reject( |TABCLASS { mv_name } from/to TRANSP not supported (SE11/SE14)| ).
        RETURN.
      ENDIF.
      IF is_transparent( ) = abap_true.
        decide_table( IMPORTING
                        ev_action   = lv_action
                        ev_modeflag = lv_modeflag
                        ev_allowed  = lv_allowed
                        ev_rows     = lv_rows
                        ev_ok       = lv_decided ).
        IF lv_decided = abap_false.
          finish( `REJECTED` ).
          RETURN.
        ENDIF.
        msg( iv_level = `I`
             iv_text = |CONVERSION { mv_name }: action={ lv_action } modeflag={ lv_modeflag } allowed={ lv_allowed } rows={ lv_rows }| ).
        IF lv_allowed = abap_false.
          reject( |Table { mv_name } ({ lv_rows } rows) needs SE14 conversion; change rejected| ).
          RETURN.
        ENDIF.
      ENDIF.
    ENDIF.
    IF check_dependents( ) = abap_false.
      finish( `REJECTED` ).
      RETURN.
    ENDIF.
    ROLLBACK WORK.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD decide_table.
    " The verdict for the table in the file.
    DATA:
      lt_dd03p TYPE ty_dd03p_tab.

    CLEAR: ev_action, ev_modeflag, ev_allowed, ev_rows, ev_ok.
    ev_rows = count_rows( mv_ddname ).
    IF ev_rows < 0.
      msg( iv_level = `E` iv_text = |The row count of { mv_name } is not available| ).
      RETURN.
    ENDIF.
    lt_dd03p = ms_file-dd03p.
    fill_types( CHANGING ct_dd03p = lt_dd03p ).
    ev_ok = decide_core( EXPORTING
                           iv_tabname  = mv_ddname
                           is_dd02v    = ms_file-dd02v
                           is_dd09l    = ms_file-dd09l
                           it_dd03p    = lt_dd03p
                           it_dd08v    = ms_file-dd08v
                         IMPORTING
                           ev_action   = ev_action
                           ev_modeflag = ev_modeflag
                           ev_allowed  = ev_allowed ).
  ENDMETHOD.


  METHOD decide_core.
    " The conversion verdict exactly as table activation decides it: the nametab of the changed
    " definition against the active nametab, DD_DECIDE in mode K without statements, and the
    " actions that TADBM allows.
    DATA:
      lt_fresh_03p TYPE ty_dd03p_tab,
      lt_fresh_08v TYPE ty_dd08v_tab,
      lt_old_03p   TYPE ty_dd03p_tab,
      lt_old_08v   TYPE ty_dd08v_tab,
      lt_x031l_new TYPE STANDARD TABLE OF x031l WITH DEFAULT KEY,
      lt_x031l_old TYPE STANDARD TABLE OF x031l WITH DEFAULT KEY,
      lt_stmt      TYPE STANDARD TABLE OF char255 WITH DEFAULT KEY.

    DATA:
      ls_x030l_new TYPE x030l,
      ls_x030l_old TYPE x030l,
      ls_dd09l     TYPE dd09l,
      ls_old_02v   TYPE dd02v,
      ls_old_09l   TYPE dd09v,
      ls_info      TYPE ddv_decideinfo.

    DATA:
      lv_subrc    TYPE sy-subrc,
      lv_errflag  TYPE c LENGTH 1,
      lv_gen_rc   TYPE sy-subrc,
      lv_old_rc   TYPE sy-subrc,
      lv_buffered TYPE abap_bool,
      lv_retyped  TYPE abap_bool,
      lv_ok       TYPE abap_bool.

    CLEAR: ev_action, ev_modeflag, ev_allowed.
    lt_fresh_03p = it_dd03p.
    lt_fresh_08v = it_dd08v.
    IF expanded( EXPORTING is_dd02v = is_dd02v
                 CHANGING  ct_dd03p = lt_fresh_03p
                           ct_dd08v = lt_fresh_08v ) = abap_false.
      RETURN.
    ENDIF.
    " The active definition, for the comparison of the keys.
    CALL FUNCTION 'DDIF_TABL_GET'
      EXPORTING
        name          = iv_tabname
        state         = 'A'
      IMPORTING
        dd02v_wa      = ls_old_02v
        dd09l_wa      = ls_old_09l
      TABLES
        dd03p_tab     = lt_old_03p
        dd08v_tab     = lt_old_08v
      EXCEPTIONS
        illegal_input = 1
        OTHERS        = 2.
    IF sy-subrc <> 0.
      lv_ok = abap_false.
    ELSE.
      lv_ok = expanded( EXPORTING is_dd02v = ls_old_02v
                        CHANGING  ct_dd03p = lt_old_03p
                                  ct_dd08v = lt_old_08v ).
    ENDIF.
    IF lv_ok = abap_false.
      msg( iv_level = `E` iv_text = |Active definition { iv_tabname } cannot be read| ).
      RETURN.
    ENDIF.
    " Transitive check: fields typed by the changed data element or domain, also inside
    " includes, get the changed type; the nametab generator derives the internal type.
    IF is_override IS NOT INITIAL.
      LOOP AT lt_fresh_03p ASSIGNING FIELD-SYMBOL(<ls_field>)
           WHERE ( rollname = is_override-rollname AND rollname IS NOT INITIAL )
              OR ( domname = is_override-domname AND domname IS NOT INITIAL ).
        <ls_field>-datatype = is_override-datatype.
        <ls_field>-leng     = is_override-leng.
        <ls_field>-decimals = is_override-decimals.
        CLEAR: <ls_field>-inttype, <ls_field>-intlen.
      ENDLOOP.
    ENDIF.
    " DD_DECIDE rewrites the active nametab header for an index action on a buffered table, so a
    " key change of a buffered table is refused without calling it. Buffered in the file or in the
    " active version counts, and a key field retyped through a data element or domain is a key change.
    lv_buffered = xsdbool( is_dd09l-bufallow <> 'N' OR ls_old_09l-bufallow <> 'N' ).
    lv_retyped = xsdbool( is_override IS NOT INITIAL
                          AND line_exists( lt_fresh_03p[ keyflag = 'X' inttype = space ] ) ).
    IF ( key_fields( lt_fresh_03p ) <> key_fields( lt_old_03p ) OR lv_retyped = abap_true )
       AND lv_buffered = abap_true.
      msg( iv_level = `E`
           iv_text = |Key { iv_tabname } changed on a buffered table: cannot be assessed without a write, rejected| ).
      RETURN.
    ENDIF.
    ls_dd09l = CORRESPONDING #( is_dd09l ).
    CALL FUNCTION 'DD_NTAB_GEN'
      EXPORTING
        dd02v_wa       = is_dd02v
        dd09l_wa       = ls_dd09l
        incl_align     = 'X'
      IMPORTING
        x030l_wa       = ls_x030l_new
      TABLES
        dd03p_tab      = lt_fresh_03p
        dd08v_tab      = lt_fresh_08v
        x031l_tab      = lt_x031l_new
      EXCEPTIONS
        wrong_datatype = 1
        wrong_tabtype  = 2
        OTHERS         = 3.
    IF sy-subrc <> 0.
      lv_gen_rc = sy-subrc.
      msg( iv_level = `E` iv_text = |Nametab of the change { iv_tabname } could not be generated (rc={ lv_gen_rc })| ).
      RETURN.
    ENDIF.
    CALL FUNCTION 'DD_GET_NAMETAB'
      EXPORTING
        status    = 'A'
        tabname   = iv_tabname
        get_all   = 'X'
      IMPORTING
        x030l_wa  = ls_x030l_old
      TABLES
        x031l_tab = lt_x031l_old
      EXCEPTIONS
        not_found = 1
        no_fields = 2
        OTHERS    = 3.
    IF sy-subrc <> 0.
      lv_old_rc = sy-subrc.
      msg( iv_level = `E` iv_text = |Active nametab { iv_tabname } cannot be read (rc={ lv_old_rc })| ).
      RETURN.
    ENDIF.
    CALL FUNCTION 'DD_DECIDE'
      EXPORTING
        action      = space
        modeflag    = space
        modus       = 'K'
        prot_id     = 0
        x030l_new   = ls_x030l_new
        x030l_old   = ls_x030l_old
        no_prog     = 'X'
        no_exec     = 'X'
      IMPORTING
        r_action    = ev_action
        r_modeflag  = ev_modeflag
        r_subrc     = lv_subrc
        errflag     = lv_errflag
        info        = ls_info
      TABLES
        pstatements = lt_stmt
        x031l_new   = lt_x031l_new
        x031l_old   = lt_x031l_old.
    IF lv_errflag = 'E'.
      msg( iv_level = `E` iv_text = |DD_DECIDE { iv_tabname } error (r_subrc={ lv_subrc })| ).
      RETURN.
    ENDIF.
    " The mapping of the activation: a MODIFY of a table without data is an ALTER.
    IF ev_action = 'MDF' AND ls_info-daction = 'ALT'.
      ev_action = 'ALT'.
    ENDIF.
    ev_allowed = xsdbool( ev_modeflag = 'A'
                          OR ( ev_modeflag = 'F' AND action_allowed( ev_action ) = abap_true ) ).
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD expanded.
    DATA:
      lv_subrc   TYPE sy-subrc,
      lv_missing TYPE ddrefstruc-flag.

    CALL FUNCTION 'DD_TABL_EXPAND'
      EXPORTING
        dd02v_wa          = is_dd02v
        mode              = 1
      IMPORTING
        subrc             = lv_subrc
        missing_table     = lv_missing
      TABLES
        dd03p_tab         = ct_dd03p
        dd08v_tab         = ct_dd08v
      EXCEPTIONS
        illegal_parameter = 1
        OTHERS            = 2.
    IF sy-subrc <> 0 OR lv_subrc > 4.
      msg( iv_level = `E` iv_text = |Expand { is_dd02v-tabname } failed (subrc={ lv_subrc })| ).
      RETURN.
    ENDIF.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD key_fields.
    LOOP AT it_dd03p INTO DATA(ls_dd03p) WHERE keyflag = 'X' AND fieldname(1) <> '.'.
      rv_keys = |{ rv_keys } { ls_dd03p-fieldname }|.
    ENDLOOP.
  ENDMETHOD.


  METHOD action_allowed.
    " TADBM as the activation reads it: a customer system uses the system name CLN.
    DATA:
      lv_sysname TYPE sy-sysid,
      lv_systype TYPE sy-sysid,
      lv_count   TYPE i.

    CALL FUNCTION 'TR_SYS_PARAMS'
      IMPORTING
        systemname    = lv_sysname
        systemtype    = lv_systype
      EXCEPTIONS
        no_systemname = 1
        no_systemtype = 2
        OTHERS        = 3.
    IF sy-subrc <> 0.
      RETURN.
    ENDIF.
    IF lv_systype <> 'SAP'.
      lv_sysname = 'CLN'.
    ENDIF.
    SELECT COUNT(*) FROM tadbm
      WHERE systname = @lv_sysname AND action = @iv_action
        AND ( uname = '*' OR uname = @sy-uname )
      INTO @lv_count.
    rv_ok = xsdbool( lv_count > 0 ).
  ENDMETHOD.


  METHOD fill_types.
    " The file keeps only the ROLLNAME of a field; the nametab generator needs the type of the
    " active data element.
    LOOP AT ct_dd03p ASSIGNING FIELD-SYMBOL(<ls_dd03p>)
         WHERE rollname IS NOT INITIAL AND datatype IS INITIAL AND fieldname(1) <> '.'.
      SELECT SINGLE datatype, leng, decimals, outputlen, lowercase, signflag, convexit, domname
        FROM dd04l
        WHERE rollname = @<ls_dd03p>-rollname AND as4local = 'A' AND as4vers = '0000'
        INTO (@<ls_dd03p>-datatype, @<ls_dd03p>-leng, @<ls_dd03p>-decimals, @<ls_dd03p>-outputlen,
              @<ls_dd03p>-lowercase, @<ls_dd03p>-signflag, @<ls_dd03p>-convexit, @<ls_dd03p>-domname).
    ENDLOOP.
  ENDMETHOD.


  METHOD check_dependents.
    " A change that reaches transparent tables through this object follows the conversion rule
    " of the table itself. A structure used by a transparent table is refused when a field
    " changes, because that cannot be assessed without writing it.
    DATA:
      lt_users  TYPE ty_users,
      lt_transp TYPE ty_users,
      lt_mine   TYPE ty_dd03p_tab,
      lt_theirs TYPE ty_dd03p_tab,
      lt_names  TYPE string_table.

    DATA:
      ls_override TYPE ty_override,
      ls_active   TYPE ty_def.

    DATA:
      lv_found    TYPE abap_bool,
      lv_old_type TYPE datatype_d,
      lv_old_leng TYPE ddleng,
      lv_old_dec  TYPE decimals,
      lv_old_dom  TYPE domname,
      lv_names    TYPE string.

    rv_ok = abap_true.
    IF mv_type = `TTYP`.
      RETURN.
    ENDIF.
    lt_users = users( ).
    LOOP AT lt_users INTO DATA(ls_user) WHERE is_transp = abap_true.
      INSERT ls_user INTO TABLE lt_transp.
    ENDLOOP.
    IF lt_transp IS INITIAL.
      RETURN.
    ENDIF.
    CASE mv_type.
      WHEN `DTEL`.
        ls_override-rollname = mv_name.
        ls_override-datatype = ms_file-dd04v-datatype.
        ls_override-leng     = ms_file-dd04v-leng.
        ls_override-decimals = ms_file-dd04v-decimals.
        IF ms_file-dd04v-domname IS NOT INITIAL.
          SELECT SINGLE datatype, leng, decimals FROM dd01l
            WHERE domname = @ms_file-dd04v-domname AND as4local = 'A' AND as4vers = '0000'
            INTO (@ls_override-datatype, @ls_override-leng, @ls_override-decimals).
        ENDIF.
      WHEN `DOMA`.
        ls_override-domname  = mv_name.
        ls_override-datatype = ms_file-dd01v-datatype.
        ls_override-leng     = ms_file-dd01v-leng.
        ls_override-decimals = ms_file-dd01v-decimals.
      WHEN `TABL`.
        get_def( EXPORTING iv_type = mv_type iv_name = mv_name IMPORTING es_def = ls_active ev_found = lv_found ).
        IF lv_found = abap_false.
          msg( iv_level = `E` iv_text = |Active definition { mv_name } cannot be read| ).
          rv_ok = abap_false.
          RETURN.
        ENDIF.
        canonicalize( CHANGING cs_def = ls_active ).
        lt_mine = ms_file-dd03p.
        LOOP AT lt_mine ASSIGNING FIELD-SYMBOL(<ls_mine>).
          CLEAR <ls_mine>-ddtext.
        ENDLOOP.
        lt_theirs = ls_active-dd03p.
        LOOP AT lt_theirs ASSIGNING FIELD-SYMBOL(<ls_theirs>).
          CLEAR <ls_theirs>-ddtext.
        ENDLOOP.
        IF lt_mine <> lt_theirs.
          LOOP AT lt_transp INTO DATA(ls_named).
            APPEND CONV string( ls_named-name ) TO lt_names.
          ENDLOOP.
          lv_names = concat_lines_of( table = lt_names sep = ` ` ).
          msg( iv_level = `E`
               iv_text = |Structure { mv_name } is used by transparent table ({ lv_names }): field change rejected| ).
          rv_ok = abap_false.
        ENDIF.
        RETURN.
      WHEN OTHERS.
        RETURN.
    ENDCASE.
    type_of_active( IMPORTING ev_datatype = lv_old_type ev_leng = lv_old_leng ev_decimals = lv_old_dec ).
    IF mv_type = `DTEL` AND ms_file-dd04v-domname IS NOT INITIAL.
      SELECT SINGLE domname FROM dd04l
        WHERE rollname = @mv_ddname AND as4local = 'A' AND as4vers = '0000'
        INTO @lv_old_dom.
      IF lv_old_dom IS NOT INITIAL.
        SELECT SINGLE datatype, leng, decimals FROM dd01l
          WHERE domname = @lv_old_dom AND as4local = 'A' AND as4vers = '0000'
          INTO (@lv_old_type, @lv_old_leng, @lv_old_dec).
      ENDIF.
    ENDIF.
    IF ls_override-datatype = lv_old_type AND ls_override-leng = lv_old_leng AND ls_override-decimals = lv_old_dec.
      RETURN.
    ENDIF.
    LOOP AT lt_transp INTO DATA(ls_tab).
      IF decide_dependent( iv_tabname = ls_tab-name is_override = ls_override ) = abap_false.
        rv_ok = abap_false.
      ENDIF.
    ENDLOOP.
  ENDMETHOD.


  METHOD decide_dependent.
    DATA:
      ls_def TYPE ty_def.

    DATA:
      lv_found    TYPE abap_bool,
      lv_ok       TYPE abap_bool,
      lv_action   TYPE tbatg-fct,
      lv_modeflag TYPE ddxtt-modeflag,
      lv_allowed  TYPE abap_bool,
      lv_rows     TYPE i.

    get_def( EXPORTING iv_type = `TABL` iv_name = |{ iv_tabname }| IMPORTING es_def = ls_def ev_found = lv_found ).
    IF lv_found = abap_false.
      msg( iv_level = `E` iv_text = |Active definition { iv_tabname } cannot be read| ).
      RETURN.
    ENDIF.
    lv_ok = decide_core( EXPORTING
                           iv_tabname  = iv_tabname
                           is_dd02v    = ls_def-dd02v
                           is_dd09l    = ls_def-dd09l
                           it_dd03p    = ls_def-dd03p
                           it_dd08v    = ls_def-dd08v
                           is_override = is_override
                         IMPORTING
                           ev_action   = lv_action
                           ev_modeflag = lv_modeflag
                           ev_allowed  = lv_allowed ).
    IF lv_ok = abap_false.
      RETURN.
    ENDIF.
    lv_rows = count_rows( iv_tabname ).
    IF lv_rows < 0.
      msg( iv_level = `E` iv_text = |The row count of { iv_tabname } is not available| ).
      RETURN.
    ENDIF.
    IF lv_allowed = abap_false.
      msg( iv_level = `E`
           iv_text = |Table { iv_tabname } ({ lv_rows } rows) needs SE14 conversion (action={ lv_action } modeflag={ lv_modeflag })| ).
      RETURN.
    ENDIF.
    msg( iv_level = `I`
         iv_text = |CONVERSION { iv_tabname }: action={ lv_action } modeflag={ lv_modeflag } allowed={ lv_allowed } rows={ lv_rows }| ).
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD type_of_active.
    CLEAR: ev_datatype, ev_leng, ev_decimals.
    CASE mv_type.
      WHEN `DTEL`.
        SELECT SINGLE datatype, leng, decimals FROM dd04l
          WHERE rollname = @mv_ddname AND as4local = 'A' AND as4vers = '0000'
          INTO (@ev_datatype, @ev_leng, @ev_decimals).
      WHEN `DOMA`.
        SELECT SINGLE datatype, leng, decimals FROM dd01l
          WHERE domname = @mv_ddname AND as4local = 'A' AND as4vers = '0000'
          INTO (@ev_datatype, @ev_leng, @ev_decimals).
    ENDCASE.
  ENDMETHOD.


  METHOD users.
    add_users( EXPORTING iv_object = CONV #( mv_type ) iv_name = mv_ddname
               CHANGING  ct_users  = rt_users ).
  ENDMETHOD.


  METHOD add_users.
    " Active users, transitively through structures and table types: data elements, tables,
    " table types, views, and search helps.
    DATA:
      lt_tab  TYPE STANDARD TABLE OF tabname WITH EMPTY KEY,
      lt_tty  TYPE STANDARD TABLE OF ttypename WITH EMPTY KEY,
      lt_dtel TYPE STANDARD TABLE OF rollname WITH EMPTY KEY,
      lt_view TYPE STANDARD TABLE OF viewname WITH EMPTY KEY,
      lt_shlp TYPE STANDARD TABLE OF shlpname WITH EMPTY KEY.

    DATA:
      lv_class TYPE dd02l-tabclass.

    CASE iv_object.
      WHEN 'DOMA'.
        SELECT DISTINCT rollname FROM dd04l
          WHERE domname = @iv_name AND as4local IN ('A','N')
          INTO TABLE @lt_dtel.
        LOOP AT lt_dtel INTO DATA(lv_dtel).
          IF NOT line_exists( ct_users[ object = 'DTEL' name = lv_dtel ] ).
            INSERT VALUE #( object = 'DTEL' name = lv_dtel ) INTO TABLE ct_users.
            " Table types and views typed by the data element.
            add_users( EXPORTING iv_object = 'DTEL' iv_name = CONV #( lv_dtel )
                       CHANGING  ct_users  = ct_users ).
          ENDIF.
        ENDLOOP.
        SELECT DISTINCT tabname FROM dd03l
          WHERE domname = @iv_name AND as4local IN ('A','N') AND fieldname NOT LIKE '.%'
          INTO TABLE @lt_tab.
      WHEN 'DTEL'.
        SELECT DISTINCT tabname FROM dd03l
          WHERE rollname = @iv_name AND as4local IN ('A','N')
          INTO TABLE @lt_tab.
        SELECT DISTINCT typename FROM dd40l
          WHERE rowtype = @iv_name AND as4local IN ('A','N')
          INTO TABLE @lt_tty.
      WHEN 'TABL' OR 'TTYP'.
        SELECT DISTINCT tabname FROM dd03l
          WHERE ( rollname = @iv_name OR ( fieldname LIKE '.%' AND precfield = @iv_name ) )
            AND as4local IN ('A','N')
          INTO TABLE @lt_tab.
        SELECT DISTINCT typename FROM dd40l
          WHERE rowtype = @iv_name AND as4local IN ('A','N')
          INTO TABLE @lt_tty.
    ENDCASE.
    " Views, foreign-key check tables, and search helps also depend on the object.
    IF iv_object = 'TABL'.
      SELECT DISTINCT viewname FROM dd26s
        WHERE tabname = @iv_name AND as4local = 'A'
        INTO TABLE @lt_view.
      SELECT DISTINCT tabname FROM dd08l
        WHERE checktable = @iv_name AND as4local = 'A' AND tabname <> @iv_name
        APPENDING TABLE @lt_tab.
    ELSEIF iv_object = 'DTEL'.
      SELECT DISTINCT viewname FROM dd27s
        WHERE rollname = @iv_name AND as4local = 'A'
        INTO TABLE @lt_view.
      SELECT DISTINCT shlpname FROM dd32s
        WHERE rollname = @iv_name AND as4local = 'A'
        INTO TABLE @lt_shlp.
    ENDIF.
    LOOP AT lt_view INTO DATA(lv_view).
      DATA(lv_view_name) = CONV ddobjname( lv_view ).
      IF NOT line_exists( ct_users[ object = 'VIEW' name = lv_view_name ] ).
        INSERT VALUE #( object = 'VIEW' name = lv_view_name ) INTO TABLE ct_users.
      ENDIF.
    ENDLOOP.
    LOOP AT lt_shlp INTO DATA(lv_shlp).
      DATA(lv_shlp_name) = CONV ddobjname( lv_shlp ).
      IF NOT line_exists( ct_users[ object = 'SHLP' name = lv_shlp_name ] ).
        INSERT VALUE #( object = 'SHLP' name = lv_shlp_name ) INTO TABLE ct_users.
      ENDIF.
    ENDLOOP.
    LOOP AT lt_tab INTO DATA(lv_tab).
      DATA(lv_tab_name) = CONV ddobjname( lv_tab ).
      IF line_exists( ct_users[ object = 'TABL' name = lv_tab_name ] ) OR lv_tab_name = mv_ddname.
        CONTINUE.
      ENDIF.
      CLEAR lv_class.
      SELECT SINGLE tabclass FROM dd02l
        WHERE tabname = @lv_tab AND as4local = 'A'
        INTO @lv_class.
      INSERT VALUE #( object = 'TABL' name = lv_tab_name is_transp = xsdbool( lv_class = 'TRANSP' ) )
        INTO TABLE ct_users.
      IF lv_class <> 'TRANSP'.
        add_users( EXPORTING iv_object = 'TABL' iv_name = lv_tab_name
                   CHANGING  ct_users  = ct_users ).
      ENDIF.
    ENDLOOP.
    LOOP AT lt_tty INTO DATA(lv_tty).
      DATA(lv_tty_name) = CONV ddobjname( lv_tty ).
      IF NOT line_exists( ct_users[ object = 'TTYP' name = lv_tty_name ] ) AND lv_tty_name <> mv_ddname.
        INSERT VALUE #( object = 'TTYP' name = lv_tty_name ) INTO TABLE ct_users.
        add_users( EXPORTING iv_object = 'TTYP' iv_name = lv_tty_name
                   CHANGING  ct_users  = ct_users ).
      ENDIF.
    ENDLOOP.
  ENDMETHOD.


  METHOD emit_guards.
    " The guards of this run, in the order the run applies them.
    fact( iv_key = `GUARD` iv_text = `Customer names only (Z or Y)` ).
    fact( iv_key = `GUARD` iv_text = `System change option, client role, and customer namespace` ).
    fact( iv_key = `GUARD` iv_text = `Request owner and state, and the package of the object` ).
    IF mv_mode = `SIMULATE`.
      fact( iv_key = `GUARD` iv_text = `Inactive version of anyone, and the base hash` ).
    ENDIF.
    IF mv_mode = `DELETE_SIM`.
      fact( iv_key = `GUARD` iv_text = `Rows of a transparent table, users outside the batch, and batch order` ).
    ELSE.
      fact( iv_key = `GUARD` iv_text = `Conversion verdict of DD_DECIDE, also for dependent tables` ).
    ENDIF.
  ENDMETHOD.


  METHOD emit_where_used.
    " Users of the object and the programs that name it. Display only: it never rejects.
    DATA:
      lt_users TYPE ty_users,
      lt_inc   TYPE STANDARD TABLE OF wbcrossgt-include WITH EMPTY KEY.

    DATA:
      lv_name  TYPE wbcrossgt-name,
      lv_count TYPE i.

    lt_users = users( ).
    LOOP AT lt_users INTO DATA(ls_user).
      lv_count = lv_count + 1.
      IF lv_count <= 50.
        msg( iv_level = `I`
             iv_text = |WHERE-USED { ls_user-object } { ls_user-name }{ COND string( WHEN ls_user-is_transp = abap_true THEN ` (transparent)` ) }| ).
      ENDIF.
    ENDLOOP.
    lv_name = mv_name.
    SELECT DISTINCT include FROM wbcrossgt
      WHERE otype = 'TY' AND name = @lv_name
      ORDER BY include
      INTO TABLE @lt_inc
      UP TO 50 ROWS.
    LOOP AT lt_inc INTO DATA(lv_inc).
      msg( iv_level = `I` iv_text = |WHERE-USED { lv_inc }| ).
      lv_count = lv_count + 1.
    ENDLOOP.
    IF lv_count = 0.
      msg( iv_level = `I` iv_text = `WHERE-USED: none` ).
    ELSEIF lv_count > 50.
      msg( iv_level = `I` iv_text = |WHERE-USED: { lv_count } users, the first 50 are listed| ).
    ENDIF.
  ENDMETHOD.


  METHOD write_definition.
    " Record in the request, PUT, activate, and prove. A failure before the PUT changes nothing.
    " A failure after it leaves the inactive version without a rollback: the file read before is
    " the way back. A create that left a definition behind is INACTIVE as well.
    IF mv_local = abap_false OR iv_create = abap_true.
      IF record_object( iv_devclass = |{ mv_devclass }|
                        iv_mode     = COND string( WHEN iv_create = abap_true THEN `I` ELSE `` ) ) = abap_false.
        RETURN.
      ENDIF.
    ENDIF.
    IF put_definition( ) = abap_false.
      IF iv_create = abap_true AND exists_object( abap_false ) = abap_true.
        finish( `INACTIVE` ).
      ELSE.
        finish( `REJECTED` ).
      ENDIF.
      RETURN.
    ENDIF.
    IF activate_definition( ) = abap_false.
      finish( `INACTIVE` ).
      RETURN.
    ENDIF.
    IF verify_definition( ) = abap_false.
      finish( `INACTIVE` ).
      RETURN.
    ENDIF.
    fact( iv_key = `RESULT_HASH` iv_text = mv_live_hash ).
    finish( `ACTIVE` ).
  ENDMETHOD.


  METHOD put_definition.
    DATA:
      lv_rc TYPE sysubrc.

    CASE mv_type.
      WHEN `DTEL`.
        CALL FUNCTION 'DDIF_DTEL_PUT'
          EXPORTING
            name              = mv_ddname
            dd04v_wa          = ms_file-dd04v
          EXCEPTIONS
            dtel_not_found    = 1
            name_inconsistent = 2
            dtel_inconsistent = 3
            put_failure       = 4
            put_refused       = 5
            OTHERS            = 6.
      WHEN `DOMA`.
        CALL FUNCTION 'DDIF_DOMA_PUT'
          EXPORTING
            name              = mv_ddname
            dd01v_wa          = ms_file-dd01v
          TABLES
            dd07v_tab         = ms_file-dd07v
          EXCEPTIONS
            doma_not_found    = 1
            name_inconsistent = 2
            doma_inconsistent = 3
            put_failure       = 4
            put_refused       = 5
            OTHERS            = 6.
      WHEN `TABL`.
        CALL FUNCTION 'DDIF_TABL_PUT'
          EXPORTING
            name              = mv_ddname
            dd02v_wa          = ms_file-dd02v
            dd09l_wa          = ms_file-dd09l
          TABLES
            dd03p_tab         = ms_file-dd03p
            dd05m_tab         = ms_file-dd05m
            dd08v_tab         = ms_file-dd08v
            dd35v_tab         = ms_file-dd35v
            dd36m_tab         = ms_file-dd36m
          EXCEPTIONS
            tabl_not_found    = 1
            name_inconsistent = 2
            tabl_inconsistent = 3
            put_failure       = 4
            put_refused       = 5
            OTHERS            = 6.
      WHEN `TTYP`.
        CALL FUNCTION 'DDIF_TTYP_PUT'
          EXPORTING
            name              = mv_ddname
            dd40v_wa          = ms_file-dd40v
          TABLES
            dd42v_tab         = ms_file-dd42v
            dd43v_tab         = ms_file-dd43v
          EXCEPTIONS
            ttyp_not_found    = 1
            name_inconsistent = 2
            ttyp_inconsistent = 3
            put_failure       = 4
            put_refused       = 5
            OTHERS            = 6.
    ENDCASE.
    IF sy-subrc <> 0.
      lv_rc = sy-subrc.
      msg( iv_level = `E` iv_text = |DDIF_{ mv_type }_PUT { mv_name } failed (rc={ lv_rc }): { sy_text( ) }| ).
      RETURN.
    ENDIF.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD activate_definition.
    DATA:
      lv_rc  TYPE sy-subrc,
      lv_exc TYPE sysubrc.

    CASE mv_type.
      WHEN `DTEL`.
        CALL FUNCTION 'DDIF_DTEL_ACTIVATE'
          EXPORTING
            name        = mv_ddname
          IMPORTING
            rc          = lv_rc
          EXCEPTIONS
            not_found   = 1
            put_failure = 2
            OTHERS      = 3.
      WHEN `DOMA`.
        CALL FUNCTION 'DDIF_DOMA_ACTIVATE'
          EXPORTING
            name        = mv_ddname
          IMPORTING
            rc          = lv_rc
          EXCEPTIONS
            not_found   = 1
            put_failure = 2
            OTHERS      = 3.
      WHEN `TABL`.
        CALL FUNCTION 'DDIF_TABL_ACTIVATE'
          EXPORTING
            name        = mv_ddname
          IMPORTING
            rc          = lv_rc
          EXCEPTIONS
            not_found   = 1
            put_failure = 2
            OTHERS      = 3.
      WHEN `TTYP`.
        CALL FUNCTION 'DDIF_TTYP_ACTIVATE'
          EXPORTING
            name        = mv_ddname
          IMPORTING
            rc          = lv_rc
          EXCEPTIONS
            not_found   = 1
            put_failure = 2
            OTHERS      = 3.
    ENDCASE.
    IF sy-subrc <> 0 OR lv_rc > 4.
      lv_exc = sy-subrc.
      msg( iv_level = `E` iv_text = |Activation of { mv_type } { mv_name } failed (rc={ lv_rc }, exc={ lv_exc })| ).
      RETURN.
    ENDIF.
    IF lv_rc = 4.
      msg( iv_level = `W` iv_text = |Activation of { mv_type } { mv_name } with warning (rc=4)| ).
    ENDIF.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD verify_definition.
    " The in-run proof: every value of the file is active (values SAP adds are accepted), a
    " transparent table has no pending adjustment of the database, and the users are active.
    DATA:
      lt_labels TYPE string_table,
      lt_users  TYPE ty_users.

    DATA:
      lv_ok       TYPE abap_bool,
      lv_diff     TYPE string,
      lv_kind     TYPE c LENGTH 1,
      lv_index    TYPE i,
      lv_pending  TYPE i,
      lv_tab      TYPE tabname,
      lv_inactive TYPE string.

    FIELD-SYMBOLS:
      <lg_file> TYPE any,
      <lg_live> TYPE any.

    canonicalize( CHANGING cs_def = ms_file ).
    IF read_live( ) = abap_false.
      msg( iv_level = `E` iv_text = |{ mv_type } { mv_name } active version not found after activation| ).
      RETURN.
    ENDIF.
    lt_labels = VALUE #( ( `DD04V` ) ( `DD01V` ) ( `DD07V` ) ( `DD02V` ) ( `DD09L` ) ( `DD03P` ) ( `DD05M` )
                         ( `DD08V` ) ( `DD35V` ) ( `DD36M` ) ( `DD40V` ) ( `DD42V` ) ( `DD43V` ) ).
    DO.
      lv_index = sy-index.
      ASSIGN COMPONENT lv_index OF STRUCTURE ms_file TO <lg_file>.
      IF sy-subrc <> 0.
        EXIT.
      ENDIF.
      ASSIGN COMPONENT lv_index OF STRUCTURE ms_live TO <lg_live>.
      DESCRIBE FIELD <lg_file> TYPE lv_kind.
      IF lv_kind = 'h'.
        subset_table( EXPORTING it_file = <lg_file> it_active = <lg_live> IMPORTING ev_ok = lv_ok ev_diff = lv_diff ).
      ELSE.
        subset_struct( EXPORTING is_file = <lg_file> is_active = <lg_live> IMPORTING ev_ok = lv_ok ev_diff = lv_diff ).
      ENDIF.
      IF lv_ok = abap_false.
        msg( iv_level = `E`
             iv_text = |{ mv_type } { mv_name } active version does not match the file after activation ({ lt_labels[ lv_index ] }): { lv_diff }| ).
        RETURN.
      ENDIF.
    ENDDO.
    IF mv_type = `TABL` AND is_transparent( ) = abap_true.
      lv_tab = mv_name.
      SELECT COUNT(*) FROM tbatg WHERE tabname = @lv_tab INTO @lv_pending.
      IF lv_pending > 0.
        msg( iv_level = `E` iv_text = |Table { mv_name } still awaiting DB adjustment (TBATG): check SE14| ).
        RETURN.
      ENDIF.
    ENDIF.
    lt_users = users( ).
    LOOP AT lt_users INTO DATA(ls_user).
      IF count_inactive( iv_type = ls_user-object iv_name = ls_user-name ) > 0.
        lv_inactive = |{ lv_inactive } { ls_user-object } { ls_user-name }|.
      ENDIF.
      " A transparent user must not be left with a pending adjustment of the database.
      IF ls_user-is_transp = abap_true.
        lv_tab = ls_user-name.
        SELECT COUNT(*) FROM tbatg WHERE tabname = @lv_tab INTO @lv_pending.
        IF lv_pending > 0.
          lv_inactive = |{ lv_inactive } TBATG { ls_user-name }|.
        ENDIF.
      ENDIF.
    ENDLOOP.
    IF lv_inactive IS NOT INITIAL.
      msg( iv_level = `E` iv_text = |Dependent objects not active after activation:{ lv_inactive }| ).
      RETURN.
    ENDIF.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD subset_struct.
    " Each component of the file equals the active one. A component left empty in the file may
    " be filled by SAP only for the attributes SAP derives or defaults itself, and for the
    " master language, which SAP sets to the original language. The first difference is named.
    DATA:
      lo_struct TYPE REF TO cl_abap_structdescr,
      lt_derived TYPE string_table.

    FIELD-SYMBOLS:
      <lv_file>   TYPE any,
      <lv_active> TYPE any.

    lt_derived = VALUE #( ( `ROWORCOLST` ) ( `INTTYPE` ) ( `INTLEN` ) ( `OUTPUTLEN` ) ( `TYPELEN` )
                          ( `DOMNAME3L` ) ( `AS4LOCAL` ) ( `AS4VERS` ) ( `DDLANGUAGE` )
                          ( `DTELMASTER` ) ( `DOMMASTER` ) ( `MASTERLANG` ) ).
    lo_struct = CAST cl_abap_structdescr( cl_abap_typedescr=>describe_by_data( is_file ) ).
    LOOP AT lo_struct->components INTO DATA(ls_comp).
      ASSIGN COMPONENT ls_comp-name OF STRUCTURE is_file TO <lv_file>.
      ASSIGN COMPONENT ls_comp-name OF STRUCTURE is_active TO <lv_active>.
      IF <lv_file> = <lv_active>.
        CONTINUE.
      ENDIF.
      IF <lv_file> IS INITIAL AND line_exists( lt_derived[ table_line = ls_comp-name ] ).
        CONTINUE.
      ENDIF.
      ev_diff = |{ ls_comp-name } file='{ <lv_file> }' active='{ <lv_active> }'|.
      RETURN.
    ENDLOOP.
    ev_ok = abap_true.
  ENDMETHOD.


  METHOD subset_table.
    " The same number of rows, compared row by row. A difference names its row.
    DATA:
      lv_ok    TYPE abap_bool,
      lv_diff  TYPE string,
      lv_index TYPE i.

    FIELD-SYMBOLS:
      <ls_file>   TYPE any,
      <ls_active> TYPE any.

    IF lines( it_file ) <> lines( it_active ).
      ev_diff = |{ lines( it_file ) } rows in the file, { lines( it_active ) } rows active|.
      RETURN.
    ENDIF.
    LOOP AT it_file ASSIGNING <ls_file>.
      lv_index = sy-tabix.
      READ TABLE it_active INDEX lv_index ASSIGNING <ls_active>.
      IF sy-subrc <> 0.
        RETURN.
      ENDIF.
      subset_struct( EXPORTING is_file = <ls_file> is_active = <ls_active> IMPORTING ev_ok = lv_ok ev_diff = lv_diff ).
      IF lv_ok = abap_false.
        ev_diff = |row { lv_index }: { lv_diff }|.
        RETURN.
      ENDIF.
    ENDLOOP.
    ev_ok = abap_true.
  ENDMETHOD.


  METHOD check_delete.
    " A transparent table with rows is never dropped, and an object that an active DDIC object
    " outside the batch still uses is not deleted. A user later in the batch would be left with a
    " dangling reference if its own delete fails, so the order is checked as well.
    DATA:
      lt_users TYPE ty_users.

    DATA:
      lv_rows  TYPE i,
      lv_self  TYPE i,
      lv_index TYPE i,
      lv_key   TYPE string,
      lv_users TYPE string,
      lv_later TYPE string.

    IF mv_type = `TABL` AND is_transparent( ) = abap_true.
      lv_rows = count_rows( mv_ddname ).
      IF lv_rows < 0.
        reject( |The row count of { mv_name } is not available| ).
        RETURN.
      ENDIF.
      IF lv_rows > 0.
        reject( |Table { mv_name } contains { lv_rows } rows; delete rejected| ).
        RETURN.
      ENDIF.
      IF mv_mode = `DELETE_SIM`.
        fact( iv_key = `ROWS` iv_text = |{ lv_rows }| ).
      ENDIF.
    ENDIF.
    lv_key = |{ mv_type }:{ mv_name }|.
    lv_self = line_index( mt_batch[ table_line = lv_key ] ).
    lt_users = users( ).
    LOOP AT lt_users INTO DATA(ls_user).
      lv_key = |{ ls_user-object }:{ ls_user-name }|.
      lv_index = line_index( mt_batch[ table_line = lv_key ] ).
      IF lv_index = 0.
        lv_users = |{ lv_users } { ls_user-object } { ls_user-name }|.
      ELSEIF lv_index > lv_self.
        lv_later = |{ lv_later } { ls_user-object } { ls_user-name }|.
      ENDIF.
    ENDLOOP.
    IF lv_later IS NOT INITIAL.
      reject( |Batch order: delete users{ lv_later } first before { mv_type } { mv_name }| ).
      RETURN.
    ENDIF.
    IF lv_users IS NOT INITIAL.
      reject( |{ mv_type } { mv_name } still in use:{ lv_users }; delete rejected| ).
      RETURN.
    ENDIF.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD delete_request.
    " A local object takes no request. An object locked in a modifiable task follows the request
    " of that task when the request is the user's own; another user's request is refused. An
    " object that is not locked needs the request of the payload.
    DATA:
      lv_object TYPE trobjtype,
      lv_obj    TYPE e071-obj_name.

    IF mv_local = abap_true.
      rv_ok = check_transport( |{ mv_devclass }| ).
      RETURN.
    ENDIF.
    lv_object = mv_type.
    lv_obj = mv_name.
    SELECT DISTINCT e070~strkorr FROM e071
      INNER JOIN e070 ON e070~trkorr = e071~trkorr
      WHERE e071~pgmid = 'R3TR' AND e071~object = @lv_object AND e071~obj_name = @lv_obj
        AND e071~lockflag = 'X' AND e070~trstatus = 'D' AND e070~trfunction <> 'T'
        AND e070~strkorr <> @space
      INTO TABLE @DATA(lt_requests).
    CASE lines( lt_requests ).
      WHEN 0.
        rv_ok = check_transport( |{ mv_devclass }| ).
      WHEN 1.
        DATA(lv_request) = lt_requests[ 1 ]-strkorr.
        SELECT SINGLE trfunction, trstatus, as4user FROM e070
          WHERE trkorr = @lv_request
          INTO @DATA(ls_e070).
        SELECT COUNT(*) FROM e070
          WHERE strkorr = @lv_request AND as4user = @sy-uname AND trstatus = 'D'
          INTO @DATA(lv_own_tasks).
        IF ls_e070-as4user <> sy-uname AND lv_own_tasks = 0.
          reject( |Object { mv_type } { mv_name } is locked in request { lv_request } of { ls_e070-as4user }| ).
          RETURN.
        ENDIF.
        IF ls_e070-trfunction <> 'K' OR ls_e070-trstatus <> 'D'.
          reject( |Object { mv_type } { mv_name } is locked in request { lv_request } ({ ls_e070-trfunction }/{ ls_e070-trstatus })| ).
          RETURN.
        ENDIF.
        IF mv_tr IS NOT INITIAL AND mv_tr <> lv_request.
          msg( iv_level = `W` iv_text = |Request { mv_tr } is ignored: the object is locked in { lv_request }| ).
        ENDIF.
        mv_tr = lv_request.
        rv_ok = abap_true.
      WHEN OTHERS.
        reject( |Object { mv_type } { mv_name } is locked in more than one request| ).
    ENDCASE.
  ENDMETHOD.


  METHOD delete_definition.
    " RS_DD_DELETE_OBJ locks by itself, so the run holds no DDIC lock here. The first proof is a
    " read in the same run after the commit; a fresh session proves it again on the Rust side.
    DATA:
      lv_objname TYPE rsedd0-ddobjname,
      lv_objtype TYPE rsedd0-ddobjtype,
      lv_tr      TYPE trkorr,
      lv_rc      TYPE sysubrc.

    lv_objname = mv_name.
    lv_objtype = SWITCH rsedd0-ddobjtype( mv_type
                   WHEN `TTYP` THEN 'A'
                   WHEN `DOMA` THEN 'D'
                   WHEN `DTEL` THEN 'E'
                   WHEN `TABL` THEN 'T' ).
    IF mv_local = abap_false.
      lv_tr = mv_tr.
    ENDIF.
    CALL FUNCTION 'RS_DD_DELETE_OBJ'
      EXPORTING
        no_ask               = 'X'
        objname              = lv_objname
        objtype              = lv_objtype
      CHANGING
        corrnum              = lv_tr
      EXCEPTIONS
        not_executed         = 1
        object_not_found     = 2
        object_not_specified = 3
        permission_failure   = 4
        dialog_needed        = 5
        error_message        = 6
        OTHERS               = 7.
    IF sy-subrc <> 0.
      lv_rc = sy-subrc.
      msg( iv_level = `E` iv_text = |RS_DD_DELETE_OBJ { mv_type } { mv_name } failed (rc={ lv_rc }): { sy_text( ) }| ).
      ROLLBACK WORK.
      IF exists_object( abap_false ) = abap_false.
        partial_delete( |Delete of { mv_type } { mv_name } may have happened in part| ).
        finish( `PARTIAL` ).
      ELSE.
        finish( `REJECTED` ).
      ENDIF.
      RETURN.
    ENDIF.
    COMMIT WORK AND WAIT.
    IF exists_object( abap_false ) = abap_true.
      still_exists( |{ mv_type } { mv_name } still exists after the delete| ).
      finish( `PARTIAL` ).
      RETURN.
    ENDIF.
    finish( `DELETED` ).
  ENDMETHOD.
