SELECT COUNT(DISTINCT customer_code) AS `成交客户数` FROM t_sales_order WHERE deleted_flag = 0 AND order_status NOT IN ('0','108','199') AND YEAR(order_time) = YEAR(CURDATE()) LIMIT 200
